//! Exercise real spill files through failure, ownership cleanup and retry.
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use reddb_server::storage::query::executors::agg_spill::{
    CountState, Mergeable, SpillCodec, SpillError, SpilledHashAgg,
};

fn batches(limit: usize) -> (tempfile::TempDir, SpilledHashAgg<u64, CountState>) {
    let directory = tempfile::tempdir().expect("private spill directory");
    let mut aggregation = SpilledHashAgg::new(directory.path(), limit, 1);
    for key in 1..=3 {
        aggregation
            .accumulate(key, CountState(key))
            .expect("accumulate");
        aggregation.spill_partition().expect("write batch");
    }
    (directory, aggregation)
}

fn assert_no_owned_files(directory: &std::path::Path) {
    let entries: Vec<_> = std::fs::read_dir(directory)
        .expect("read private directory")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect();
    assert!(entries.is_empty(), "owned spill files remain: {entries:?}");
}

#[test]
fn oversized_batch_error_removes_every_owned_batch() {
    let (directory, aggregation) = batches(8);
    assert!(matches!(
        aggregation.drain(),
        Err(SpillError::BatchTooLarge { .. })
    ));
    assert_no_owned_files(directory.path());
}

#[test]
fn truncated_batches_are_errors_and_release_all_owned_files() {
    for length in 0..16 {
        let (directory, aggregation) = batches(64);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(directory.path().join("spill_0.bin"))
            .expect("owned first batch");
        file.set_len(length).expect("truncate private batch");
        drop(file);
        assert!(
            aggregation.drain().is_err(),
            "truncation to {length} bytes must not lose an acknowledged group silently"
        );
        assert_no_owned_files(directory.path());
    }
}

#[test]
fn primitive_codecs_distinguish_empty_input_from_partial_headers() {
    assert_eq!(u64::decode(&mut &[][..]).expect("clean EOF"), None);
    assert_eq!(String::decode(&mut &[][..]).expect("clean EOF"), None);
    for length in 1..8 {
        assert!(
            u64::decode(&mut &vec![0_u8; length][..]).is_err(),
            "partial u64 header {length}"
        );
        assert!(
            i64::decode(&mut &vec![0_u8; length][..]).is_err(),
            "partial i64 header {length}"
        );
        assert!(
            f64::decode(&mut &vec![0_u8; length][..]).is_err(),
            "partial f64 header {length}"
        );
    }
    for length in 1..4 {
        assert!(
            String::decode(&mut &vec![0_u8; length][..]).is_err(),
            "partial string header {length}"
        );
    }
}

#[derive(Clone, Debug)]
struct FlakyState {
    value: u64,
    fail: Arc<AtomicBool>,
}

impl Mergeable for FlakyState {
    fn merge(&mut self, other: Self) {
        self.value += other.value;
    }
}

impl SpillCodec for FlakyState {
    fn encode<W: Write>(&self, writer: &mut W) -> Result<usize, SpillError> {
        if self.fail.load(Ordering::Relaxed) {
            writer.write_all(&[1, 2, 3])?;
            return Err(SpillError::Codec("injected encoder failure".into()));
        }
        self.value.encode(writer)
    }

    fn decode<R: Read>(reader: &mut R) -> Result<Option<Self>, SpillError> {
        Ok(u64::decode(reader)?.map(|value| Self {
            value,
            fail: Arc::new(AtomicBool::new(false)),
        }))
    }
    fn decode_with_budget<R: Read>(
        reader: &mut R,
        budget: &mut reddb_server::storage::query::executors::agg_spill::SpillDecodeBudget,
    ) -> Result<Option<Self>, SpillError> {
        budget.charge_bytes(32)?;
        Self::decode(reader)
    }
}

#[test]
fn failed_encode_removes_partial_file_before_aggregation_drop() {
    let directory = tempfile::tempdir().expect("private directory");
    let mut aggregation = SpilledHashAgg::new(directory.path(), 1024, 1);
    aggregation
        .accumulate(
            7_u64,
            FlakyState {
                value: 5,
                fail: Arc::new(AtomicBool::new(true)),
            },
        )
        .expect("accumulate");
    assert!(aggregation.spill_partition().is_err());
    assert_no_owned_files(directory.path());
    drop(aggregation);
    assert_no_owned_files(directory.path());
}

#[test]
fn failed_spill_preserves_groups_for_an_exact_retry() {
    let directory = tempfile::tempdir().expect("private directory");
    let fail = Arc::new(AtomicBool::new(true));
    let mut aggregation = SpilledHashAgg::new(directory.path(), 1024, 1);
    aggregation
        .accumulate(
            7_u64,
            FlakyState {
                value: 5,
                fail: Arc::clone(&fail),
            },
        )
        .expect("accumulate");
    assert!(aggregation.spill_partition().is_err());
    assert_eq!(
        aggregation.in_memory_groups(),
        1,
        "failure must preserve the pre-spill group"
    );
    fail.store(false, Ordering::Relaxed);
    aggregation.spill_partition().expect("retry encoding");
    let merged = aggregation.drain().expect("merge retried spill");
    assert_eq!(merged.len(), 1);
    assert_eq!(merged.get(&7).expect("original key").value, 5);
    assert_no_owned_files(directory.path());
}

#[test]
fn failed_exclusive_create_preserves_a_file_the_aggregation_does_not_own() {
    let directory = tempfile::tempdir().expect("private directory");
    let path = directory.path().join("spill_0.bin");
    std::fs::write(&path, b"preexisting-file").expect("fixture collision");
    let mut aggregation = SpilledHashAgg::new(directory.path(), 1024, 1);
    aggregation
        .accumulate(7_u64, CountState(5))
        .expect("accumulate");
    assert!(aggregation.spill_partition().is_err());
    drop(aggregation);
    assert_eq!(
        std::fs::read(&path).expect("unowned file remains"),
        b"preexisting-file"
    );
}

#[test]
fn repeated_failures_and_successes_reuse_one_directory_without_growth() {
    let directory = tempfile::tempdir().expect("reused private directory");
    for cycle in 0..120 {
        let limit = if cycle % 3 == 0 { 8 } else { 64 };
        let mut aggregation = SpilledHashAgg::new(directory.path(), limit, 1);
        for key in 1_u64..=3 {
            aggregation
                .accumulate(key, CountState(key))
                .expect("new group");
            aggregation.spill_partition().expect("owned batch");
        }
        if cycle % 3 == 1 {
            std::fs::OpenOptions::new()
                .write(true)
                .open(directory.path().join("spill_0.bin"))
                .expect("owned batch")
                .set_len(11)
                .expect("inject interrupted state");
        }
        match cycle % 3 {
            0 | 1 => assert!(aggregation.drain().is_err(), "fault cycle {cycle}"),
            _ => {
                let groups = aggregation.drain().expect("successful merge");
                assert_eq!(groups.len(), 3);
                assert_eq!(groups.values().map(|state| state.0).sum::<u64>(), 6);
            }
        }
        assert_no_owned_files(directory.path());
    }
}

#[test]
fn decoders_accept_short_reads_and_interrupted_reads_without_losing_records() {
    struct ShortReader<'a> {
        bytes: &'a [u8],
        interrupt: bool,
    }
    impl Read for ShortReader<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupt {
                self.interrupt = false;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            if out.is_empty() || self.bytes.is_empty() {
                return Ok(0);
            }
            out[0] = self.bytes[0];
            self.bytes = &self.bytes[1..];
            Ok(1)
        }
    }
    let encoded = 0x0102_0304_0506_0708_u64.to_le_bytes();
    let mut reader = ShortReader {
        bytes: &encoded,
        interrupt: true,
    };
    assert_eq!(
        u64::decode(&mut reader).expect("complete short reads"),
        Some(0x0102_0304_0506_0708)
    );
    assert_eq!(u64::decode(&mut reader).expect("clean EOF"), None);
    let mut reader = ShortReader {
        bytes: &encoded[..7],
        interrupt: true,
    };
    assert!(u64::decode(&mut reader).is_err());
    let mut encoded = Vec::new();
    "abc"
        .to_string()
        .encode(&mut encoded)
        .expect("string record");
    let mut reader = ShortReader {
        bytes: &encoded,
        interrupt: true,
    };
    assert_eq!(
        String::decode(&mut reader).expect("string short reads"),
        Some("abc".to_string())
    );
    assert_eq!(String::decode(&mut reader).expect("string EOF"), None);
}

#[test]
fn length_admission_precedes_string_allocation_and_rejects_count_overflow() {
    use reddb_server::storage::query::executors::agg_spill::SpillDecodeBudget;
    let mut budget = SpillDecodeBudget::new(4096);
    let prefix = 100_000_u32.to_le_bytes();
    let error = String::decode_with_budget(&mut &prefix[..], &mut budget)
        .expect_err("oversized string length");
    assert!(error.to_string().contains("allocation limit"), "{error}");
    assert_eq!(budget.remaining_bytes(), 4096);
    assert!(budget.charge_elements::<u64>(usize::MAX).is_err());
    assert_eq!(budget.remaining_bytes(), 4096);
}

#[test]
fn spill_drain_does_not_fall_back_to_an_unbounded_custom_decoder() {
    #[derive(Clone)]
    struct LegacyState;
    impl Mergeable for LegacyState {
        fn merge(&mut self, _other: Self) {}
    }
    impl SpillCodec for LegacyState {
        fn encode<W: Write>(&self, writer: &mut W) -> Result<usize, SpillError> {
            1_u64.encode(writer)
        }
        fn decode<R: Read>(_reader: &mut R) -> Result<Option<Self>, SpillError> {
            panic!("unbounded legacy decoder must not run")
        }
    }
    let directory = tempfile::tempdir().expect("private legacy spill");
    let mut aggregation = SpilledHashAgg::new(directory.path(), 1024, 1);
    aggregation
        .accumulate(1_u64, LegacyState)
        .expect("accumulate");
    aggregation.spill_partition().expect("encode legacy state");
    match aggregation.drain() {
        Err(SpillError::Codec(message)) => {
            assert!(message.contains("bounded spill decoding"), "{message}")
        }
        _ => panic!("unsupported bounded codec must fail before invoking the legacy decoder"),
    }
    assert_no_owned_files(directory.path());
}
