//! Write → persist → recover **value-level** equivalence harness (DST #1356).
//!
//! Pillar B (equivalence testing). Where [`crate::wal_workload`] writes opaque
//! random page bytes to exercise the *structural* recovery invariants, this
//! workload writes **typed values spanning every supported [`Value`] variant**,
//! each encoded with the engine's real [`reddb_types::value_codec`]. After a
//! forced crash + recovery, the decoded committed values must equal *exactly*
//! the values that were durably committed before the crash — value-level
//! equivalence layered directly on top of the structural oracle in
//! [`crate::oracle`].
//!
//! Determinism: every choice (corpus permutation, transaction sizes, checkpoint
//! cadence) is derived from a single seed, so the committed-value *sequence* is
//! reproducible. A crash recovers the longest valid prefix, so the recovered
//! committed values are always an exact prefix of the seed's full model — any
//! discovered mismatch reproduces via `SEED=<n>`.

use crate::prng::SplitMix64;
use crate::superblock::{self, Superblock};
use crate::wal_workload::{SUPERBLOCK_FILE_NAME, WAL_FILE_NAME};
use reddb_file::wal_header::{
    decode_wal_file_header, encode_wal_file_header, WAL_FILE_HEADER_BYTES,
};
use reddb_file::wal_record::{
    decode_main_wal_record_frame, encode_main_wal_record_frame, MainWalRecordFrame,
};
use reddb_types::{value_codec, Value};
use std::fs::{File, OpenOptions};
use std::io::{self, Cursor, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

/// The fixed term stamped on every record (term fencing is a later DST slice).
const WORKLOAD_TERM: u64 = 1;

/// One transaction the workload durably committed, in write order.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedTx {
    /// The transaction id (also the committed LSN, strictly increasing).
    pub tx_id: u64,
    /// The typed values written by this transaction, in page order.
    pub values: Vec<Value>,
    /// Byte offset of the end of this transaction's `Commit` record. A crash
    /// that truncates the WAL at `cut` keeps this transaction iff
    /// `commit_end_offset <= cut`.
    pub commit_end_offset: u64,
}

/// The full, fault-free model the workload would persist for a given seed: the
/// ordered sequence of committed transactions plus the WAL format version.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedModel {
    /// Committed transactions in write order.
    pub txs: Vec<CommittedTx>,
    /// The WAL file format version the workload wrote.
    pub format_version: u8,
}

impl TypedModel {
    /// The transactions that remain committed after a crash truncating the WAL
    /// to `surviving_bytes`: those whose `Commit` record landed fully within the
    /// surviving prefix.
    pub fn committed_through(&self, surviving_bytes: u64) -> Vec<RecoveredTx> {
        self.txs
            .iter()
            .filter(|tx| tx.commit_end_offset <= surviving_bytes)
            .map(|tx| RecoveredTx {
                tx_id: tx.tx_id,
                values: tx.values.clone(),
            })
            .collect()
    }

    /// The full committed sequence as [`RecoveredTx`] (no crash).
    pub fn all_committed(&self) -> Vec<RecoveredTx> {
        self.committed_through(u64::MAX)
    }

    /// The highest committed LSN in the fault-free model.
    pub fn last_committed_lsn(&self) -> u64 {
        self.txs.last().map_or(0, |tx| tx.tx_id)
    }
}

/// A transaction reconstructed from the recovered WAL prefix.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredTx {
    /// The recovered transaction id.
    pub tx_id: u64,
    /// The decoded typed values, in page order.
    pub values: Vec<Value>,
}

/// A value-equivalence recovery failure: the recovered committed state cannot be
/// reconstructed because a CRC-valid committed record carried undecodable typed
/// bytes (a real durability/codec bug, not an expected torn tail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EquivalenceError {
    /// The WAL file could not be read.
    WalUnreadable(String),
    /// A CRC-valid `PageWrite` in a committed transaction held bytes the value
    /// codec rejected — committed data must always decode.
    UndecodableCommittedValue { tx_id: u64, page_index: usize },
}

impl std::fmt::Display for EquivalenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WalUnreadable(e) => write!(f, "WAL unreadable: {e}"),
            Self::UndecodableCommittedValue { tx_id, page_index } => write!(
                f,
                "committed tx {tx_id} page {page_index} held undecodable value bytes"
            ),
        }
    }
}

impl std::error::Error for EquivalenceError {}

/// Every supported [`Value`] variant, each as a representative instance that
/// round-trips byte-faithfully through [`value_codec`]. This is the corpus the
/// harness persists; its union across the committed transactions covers all
/// supported types. Mirrors the registry round-trip corpus in `reddb-types`.
pub fn canonical_value_corpus() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Integer(-1),
        Value::UnsignedInteger(2),
        Value::Float(3.5),
        Value::text("hello"),
        Value::Blob(vec![1, 2, 3]),
        Value::Boolean(true),
        Value::Timestamp(4),
        Value::Duration(5),
        Value::IpAddr(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
        Value::IpAddr(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        Value::MacAddr([1, 2, 3, 4, 5, 6]),
        Value::Vector(vec![1.0, 2.0]),
        Value::Json(br#"{"ok":true}"#.to_vec()),
        Value::Uuid([7; 16]),
        Value::NodeRef("node".to_string()),
        Value::EdgeRef("edge".to_string()),
        Value::VectorRef("vectors".to_string(), 8),
        Value::RowRef("rows".to_string(), 9),
        Value::Color([0xAA, 0xBB, 0xCC]),
        Value::Email("a@example.com".to_string()),
        Value::Url("https://example.com".to_string()),
        Value::Phone(5_511_999),
        Value::Semver(1_002_003),
        Value::Cidr(10 << 24, 8),
        Value::Date(20_000),
        Value::Time(43_200_000),
        Value::Decimal(123_456),
        Value::EnumValue(3),
        Value::Array(vec![Value::Integer(1), Value::text("two")]),
        Value::TimestampMs(123_456),
        Value::Ipv4(0x7f00_0001),
        Value::Ipv6([1; 16]),
        Value::Subnet(10 << 24, 0xff00_0000),
        Value::Port(5432),
        Value::Latitude(-23_550_520),
        Value::Longitude(-46_633_308),
        Value::GeoPoint(-23_550_520, -46_633_308),
        Value::Country2(*b"BR"),
        Value::Country3(*b"BRA"),
        Value::Lang2(*b"pt"),
        Value::Lang5(*b"pt-BR"),
        Value::Currency(*b"USD"),
        Value::AssetCode("BTC".to_string()),
        Value::Money {
            asset_code: "USD".to_string(),
            minor_units: 1234,
            scale: 2,
        },
        Value::ColorAlpha([1, 2, 3, 4]),
        Value::BigInt(-10),
        Value::KeyRef("kv".to_string(), "key".to_string()),
        Value::DocRef("docs".to_string(), 42),
        Value::TableRef("users".to_string()),
        Value::PageRef(99),
        Value::Secret(vec![9, 8, 7]),
        Value::Password("$argon2id$v=19$hash".to_string()),
    ]
}

/// Drive the typed-value workload in `dir`, deriving every choice from `seed`.
///
/// Persists the full corpus across a deterministic sequence of `Begin` →
/// `PageWrite`(typed value)* → `Commit` transactions, each commit followed by an
/// `fsync`, with periodic checkpoints stamping the dual superblock. Returns the
/// fault-free [`TypedModel`] — the ground truth a crashed recovery is compared
/// against. The bytes written are identical to the `typed_workload` binary for
/// the same seed, so a recovered prefix is always a prefix of this model.
pub fn run_typed_workload(dir: &Path, seed: u64) -> io::Result<TypedModel> {
    let mut rng = SplitMix64::new(seed ^ 0x5641_4C5F_4551); // "VAL_EQ"

    // A deterministic permutation of the corpus, so different seeds land
    // different variants at different crash boundaries while still covering all.
    let mut corpus = canonical_value_corpus();
    shuffle(&mut corpus, &mut rng);

    let wal_path = dir.join(WAL_FILE_NAME);
    let sb_path = dir.join(SUPERBLOCK_FILE_NAME);

    let mut wal = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&wal_path)?;
    wal.write_all(&encode_wal_file_header())?;
    wal.sync_all()?;

    let mut offset = WAL_FILE_HEADER_BYTES as u64;
    let checkpoint_interval = 3 + rng.below(3);

    let mut txs = Vec::new();
    let mut generation = 0u64;
    let mut next_page_id = 0u64;
    let mut tx_id = 0u64;
    let mut cursor = 0usize;

    while cursor < corpus.len() {
        tx_id += 1;
        let remaining = corpus.len() - cursor;
        let take = (1 + usize::try_from(rng.below(4)).unwrap_or(0)).min(remaining);
        let values: Vec<Value> = corpus[cursor..cursor + take].to_vec();
        cursor += take;

        append_frame(&mut wal, &mut offset, &MainWalRecordFrame::Begin { tx_id })?;
        for value in &values {
            let mut data = Vec::new();
            value_codec::encode(value, &mut data);
            let page_id = u32::try_from(next_page_id).unwrap_or(u32::MAX);
            next_page_id += 1;
            append_frame(
                &mut wal,
                &mut offset,
                &MainWalRecordFrame::PageWrite {
                    tx_id,
                    page_id,
                    data,
                },
            )?;
        }
        append_frame(&mut wal, &mut offset, &MainWalRecordFrame::Commit { tx_id })?;
        // The commit is durable only once the fsync returns success.
        wal.sync_all()?;
        let commit_end_offset = offset;

        txs.push(CommittedTx {
            tx_id,
            values,
            commit_end_offset,
        });

        if tx_id.is_multiple_of(checkpoint_interval) {
            append_frame(
                &mut wal,
                &mut offset,
                &MainWalRecordFrame::Checkpoint { lsn: tx_id },
            )?;
            wal.sync_all()?;
            write_superblock(&sb_path, generation, tx_id)?;
            generation += 1;
        }
    }

    // Final checkpoint so the superblock reflects the last commit.
    write_superblock(&sb_path, generation, tx_id)?;

    Ok(TypedModel {
        txs,
        format_version: reddb_file::wal_header::WAL_FILE_VERSION,
    })
}

/// Recover the committed typed values from the WAL in `dir`, replaying the
/// longest valid prefix and decoding every committed `PageWrite` payload back to
/// a [`Value`]. Only fully `Begin..Commit`-bracketed transactions are returned;
/// a torn or uncommitted trailing transaction is dropped, exactly as the
/// engine's recovery would discard it.
pub fn recover_committed_values(dir: &Path) -> Result<Vec<RecoveredTx>, EquivalenceError> {
    let wal_bytes = match std::fs::read(dir.join(WAL_FILE_NAME)) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(EquivalenceError::WalUnreadable(err.to_string())),
    };

    if wal_bytes.len() < WAL_FILE_HEADER_BYTES {
        return Ok(Vec::new());
    }
    let mut header = [0u8; WAL_FILE_HEADER_BYTES];
    header.copy_from_slice(&wal_bytes[..WAL_FILE_HEADER_BYTES]);
    let version = match decode_wal_file_header(&header) {
        Ok(h) => h.version,
        Err(_) => return Ok(Vec::new()),
    };

    let mut reader = Cursor::new(&wal_bytes);
    reader.set_position(WAL_FILE_HEADER_BYTES as u64);

    let mut committed = Vec::new();
    let mut open: Option<(u64, Vec<Value>)> = None;

    // Replay until a clean EOF or a torn trailing record ends the prefix.
    while let Ok(Some((_term, frame))) =
        decode_main_wal_record_frame(&mut reader, version, WORKLOAD_TERM)
    {
        match frame {
            MainWalRecordFrame::Begin { tx_id } => open = Some((tx_id, Vec::new())),
            MainWalRecordFrame::PageWrite { tx_id, data, .. } => {
                if let Some((open_id, values)) = open.as_mut() {
                    if *open_id == tx_id {
                        let page_index = values.len();
                        match value_codec::decode(&data) {
                            Ok((value, _consumed)) => values.push(value),
                            Err(_) => {
                                return Err(EquivalenceError::UndecodableCommittedValue {
                                    tx_id,
                                    page_index,
                                })
                            }
                        }
                    }
                }
            }
            MainWalRecordFrame::Commit { tx_id } => {
                if let Some((open_id, values)) = open.take() {
                    if open_id == tx_id {
                        committed.push(RecoveredTx { tx_id, values });
                    }
                }
            }
            // Checkpoints and the frames the workload never emits are
            // structurally validated by the oracle; ignore them here.
            _ => {}
        }
    }

    Ok(committed)
}

/// Fisher-Yates shuffle driven entirely by the seeded PRNG, so the corpus
/// permutation is reproducible for a given seed.
fn shuffle(values: &mut [Value], rng: &mut SplitMix64) {
    let len = values.len();
    for i in (1..len).rev() {
        let j = usize::try_from(rng.below((i + 1) as u64)).unwrap_or(0);
        values.swap(i, j);
    }
}

fn append_frame(wal: &mut File, offset: &mut u64, frame: &MainWalRecordFrame) -> io::Result<()> {
    // Encode to a single buffer and write it in one `write_all`, so a short
    // write torns within a record boundary rather than between fields.
    let bytes = encode_main_wal_record_frame(frame, WORKLOAD_TERM)?;
    wal.write_all(&bytes)?;
    *offset += bytes.len() as u64;
    Ok(())
}

fn write_superblock(path: &Path, generation: u64, committed_lsn: u64) -> io::Result<()> {
    let slot = Superblock {
        generation,
        committed_lsn,
    }
    .encode();
    // Do not truncate: each checkpoint overwrites only its own slot and must
    // preserve the other slot's durable copy.
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.seek(SeekFrom::Start(superblock::slot_offset(generation)))?;
    file.write_all(&slot)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_covers_every_value_variant_round_trip() {
        // Every representative value must survive the same codec the workload
        // uses, else equivalence is meaningless.
        for original in canonical_value_corpus() {
            let mut bytes = Vec::new();
            value_codec::encode(&original, &mut bytes);
            let (decoded, consumed) = value_codec::decode(&bytes).expect("decode");
            assert_eq!(consumed, bytes.len());
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn fault_free_run_recovers_every_committed_value() {
        let dir = tempfile::tempdir().unwrap();
        let model = run_typed_workload(dir.path(), 2024).unwrap();
        // The union of all committed values is the full corpus (every type).
        let mut union = Vec::new();
        for tx in &model.txs {
            union.extend(tx.values.iter().cloned());
        }
        assert_eq!(union.len(), canonical_value_corpus().len());

        let recovered = recover_committed_values(dir.path()).unwrap();
        assert_eq!(recovered, model.all_committed());
    }

    #[test]
    fn same_seed_produces_identical_model_and_bytes() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let model_a = run_typed_workload(dir_a.path(), 777).unwrap();
        let model_b = run_typed_workload(dir_b.path(), 777).unwrap();
        assert_eq!(model_a, model_b);
        let wal_a = std::fs::read(dir_a.path().join(WAL_FILE_NAME)).unwrap();
        let wal_b = std::fs::read(dir_b.path().join(WAL_FILE_NAME)).unwrap();
        assert_eq!(wal_a, wal_b, "same seed must yield byte-identical WAL");
    }

    #[test]
    fn truncated_wal_recovers_exactly_the_committed_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let model = run_typed_workload(dir.path(), 55).unwrap();
        let wal_bytes = std::fs::read(dir.path().join(WAL_FILE_NAME)).unwrap();

        // Truncating at each transaction's commit boundary (and one byte before
        // it) must recover exactly the model prefix that survived the cut.
        for tx in &model.txs {
            for cut in [tx.commit_end_offset, tx.commit_end_offset - 1] {
                let crashed = tempfile::tempdir().unwrap();
                let truncated = &wal_bytes[..usize::try_from(cut).unwrap()];
                std::fs::write(crashed.path().join(WAL_FILE_NAME), truncated).unwrap();
                let recovered = recover_committed_values(crashed.path()).unwrap();
                assert_eq!(
                    recovered,
                    model.committed_through(cut),
                    "value equivalence broke at cut={cut}"
                );
            }
        }
    }

    #[test]
    fn empty_dir_recovers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(recover_committed_values(dir.path()).unwrap(), Vec::new());
    }
}
