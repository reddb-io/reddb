use reddb_file::append_only_segment::streaming::StreamingAppendOnlySegment;
use reddb_file::{
    append_only_segment_chunk_checksums, encode_append_only_segment, AppendOnlySegmentCodec,
    AppendOnlySegmentRow, OpenMode, StdVfs, Vfs,
};

fn read_in_steps(
    codec: AppendOnlySegmentCodec,
    rows: &[AppendOnlySegmentRow],
    file_budget: usize,
    plain_budget: usize,
    row_budget: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("segment.raos");
    let bytes = encode_append_only_segment(codec, rows).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let file = StdVfs.open(&path, OpenMode::read_only()).unwrap();
    let mut reader = StreamingAppendOnlySegment::new(
        file,
        append_only_segment_chunk_checksums(&bytes),
        1024 * 1024,
    )
    .unwrap();
    let mut actual = Vec::new();
    for _ in 0..1_000_000 {
        let progress = reader
            .step(file_budget, plain_budget, row_budget, |row| {
                actual.push(row);
                Ok(())
            })
            .unwrap();
        assert!(progress.file_bytes <= file_budget);
        assert!(progress.plain_bytes <= plain_budget);
        assert!(progress.rows <= row_budget);
        if progress.complete {
            assert_eq!(actual, rows);
            return;
        }
        assert!(
            progress.file_bytes > 0 || progress.plain_bytes > 0 || progress.rows > 0,
            "stream stalled"
        );
    }
    panic!("stream did not complete");
}

#[test]
fn incremental_segment_reader_handles_single_byte_ticks() {
    let rows = vec![
        AppendOnlySegmentRow {
            primary_key: vec![1],
            payload: b"first".to_vec(),
        },
        AppendOnlySegmentRow {
            primary_key: vec![2],
            payload: b"second".to_vec(),
        },
    ];
    for codec in [AppendOnlySegmentCodec::None, AppendOnlySegmentCodec::Zstd] {
        read_in_steps(codec, &rows, 1, 1, 1);
    }
}

#[test]
fn incremental_segment_reader_reaches_rows_in_large_compressed_files() {
    let mut random = 0x1234_5678_9abc_def0_u64;
    let rows = (0_u64..12_000)
        .map(|id| {
            let payload = (0..128)
                .map(|_| {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    random as u8
                })
                .collect();
            AppendOnlySegmentRow {
                primary_key: id.to_be_bytes().to_vec(),
                payload,
            }
        })
        .collect::<Vec<_>>();
    let bytes = encode_append_only_segment(AppendOnlySegmentCodec::Zstd, &rows).unwrap();
    assert!(
        bytes.len() > 1024 * 1024,
        "fixture exceeds the previous compressed-file limit"
    );
    read_in_steps(AppendOnlySegmentCodec::Zstd, &rows, 8192, 8192, 9);
}

#[test]
fn incremental_segment_reader_never_completes_on_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("segment.raos");
    let rows = vec![AppendOnlySegmentRow {
        primary_key: b"1".to_vec(),
        payload: b"old".to_vec(),
    }];
    let mut bytes = encode_append_only_segment(AppendOnlySegmentCodec::Zstd, &rows).unwrap();
    let checksums = append_only_segment_chunk_checksums(&bytes);
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    let file = StdVfs.open(&path, OpenMode::read_only()).unwrap();
    let mut reader = StreamingAppendOnlySegment::new(file, checksums, 1024).unwrap();
    let result = loop {
        match reader.step(17, 31, 1, |_| Ok(())) {
            Ok(progress) if !progress.complete => continue,
            other => break other,
        }
    };
    assert!(
        result.is_err(),
        "corruption must not produce a verified stream"
    );
}

fn replace_uncompressed_body(original: &[u8], body: &[u8]) -> Vec<u8> {
    let header_len = u32::from_le_bytes(original[8..12].try_into().unwrap()) as usize;
    let mut header: serde_json::Value =
        serde_json::from_slice(&original[28..28 + header_len]).unwrap();
    header["uncompressed_len"] = serde_json::Value::from(body.len() as u64);
    header["chunk_checksums"] =
        serde_json::to_value(append_only_segment_chunk_checksums(body)).unwrap();
    let header = serde_json::to_vec(&header).unwrap();
    let mut bytes = original[..8].to_vec();
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(body.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());
    bytes.extend_from_slice(&crc32fast::hash(body).to_le_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(body);
    bytes
}

fn assert_stream_rejects(bytes: &[u8], row_limit: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad.raos");
    std::fs::write(&path, bytes).unwrap();
    let file = StdVfs.open(&path, OpenMode::read_only()).unwrap();
    let mut reader = StreamingAppendOnlySegment::new(
        file,
        append_only_segment_chunk_checksums(bytes),
        row_limit,
    )
    .unwrap();
    for _ in 0..100_000 {
        match reader.step(7, 11, 1, |_| Ok(())) {
            Err(_) => return,
            Ok(progress) => {
                assert!(
                    !progress.complete,
                    "malformed input must never become verified"
                );
                assert!(progress.file_bytes > 0 || progress.plain_bytes > 0 || progress.rows > 0);
            }
        }
    }
    panic!("invalid stream did not terminate");
}

#[test]
fn incremental_segment_reader_rejects_malformed_json_and_oversized_rows() {
    let row = AppendOnlySegmentRow {
        primary_key: b"1".to_vec(),
        payload: vec![42; 100],
    };
    let original = encode_append_only_segment(AppendOnlySegmentCodec::None, &[row]).unwrap();
    for body in [
        b"[\x0b{\"primary_key_hex\":\"31\",\"payload_hex\":\"00\"}]".as_slice(),
        b"[{\"primary_key_hex\":\"31\",\"payload_hex\":\"00\"},]".as_slice(),
        b"[{\"primary_key_hex\":\"31\",\"payload_hex\":\"0\"}]".as_slice(),
    ] {
        assert_stream_rejects(&replace_uncompressed_body(&original, body), 1024);
    }
    assert_stream_rejects(&original, 32);
}

#[test]
fn incremental_segment_reader_checks_frame_crc_even_when_outer_checksums_match() {
    let row = AppendOnlySegmentRow {
        primary_key: b"1".to_vec(),
        payload: b"payload".to_vec(),
    };
    let mut bytes = encode_append_only_segment(AppendOnlySegmentCodec::None, &[row]).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    assert_stream_rejects(&bytes, 1024);
}
