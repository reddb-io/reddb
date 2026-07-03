//! TM commit-path DST workload (#1651).
//!
//! This campaign stays in the existing `unreliable-libc` lane: each scenario
//! writes real RedDB WAL frames through the same `Vfs` abstraction as the
//! storage-fault suites, then recovery first runs the shared structural oracle
//! and finally checks TM-specific commit semantics against the recovered prefix.

use crate::prng::SplitMix64;
use crate::vfs::{OpenMode, StdVfs, Vfs, VfsFile};
use crate::wal_workload::WAL_FILE_NAME;
use reddb_file::wal_header::{
    decode_wal_file_header, encode_wal_file_header, WAL_FILE_HEADER_BYTES,
};
use reddb_file::wal_record::{
    decode_main_wal_record_frame, encode_main_wal_record_frame, MainWalRecordFrame,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Cursor};
use std::path::Path;

const WORKLOAD_TERM: u64 = 1;

/// Commit-path scenario families required by #1651.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmCommitPathScenario {
    FcwBeforeWalAppend,
    WalAppendBeforeFinalize,
    SavepointReleaseRollback,
    ConcurrentWriters,
}

impl TmCommitPathScenario {
    pub fn all() -> [Self; 4] {
        [
            Self::FcwBeforeWalAppend,
            Self::WalAppendBeforeFinalize,
            Self::SavepointReleaseRollback,
            Self::ConcurrentWriters,
        ]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::FcwBeforeWalAppend => "fcw_before_wal_append",
            Self::WalAppendBeforeFinalize => "wal_append_before_finalize",
            Self::SavepointReleaseRollback => "savepoint_release_rollback",
            Self::ConcurrentWriters => "concurrent_writers",
        }
    }
}

/// Stable scenario names for docs/tests.
pub fn tm_commit_path_scenarios() -> [&'static str; 4] {
    TmCommitPathScenario::all().map(TmCommitPathScenario::name)
}

/// Fault-free model for one scenario/seed. `cut`-based tests use the recorded
/// commit offsets to decide whether a transaction should be present or absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmCommitPathModel {
    pub scenario: TmCommitPathScenario,
    pub txs: Vec<TmCommittedTx>,
    pub wal_len: u64,
}

impl TmCommitPathModel {
    pub fn committed_through(&self, surviving_bytes: u64) -> Vec<TmRecoveredTx> {
        self.txs
            .iter()
            .filter(|tx| tx.commit_end_offset <= surviving_bytes)
            .map(|tx| TmRecoveredTx {
                tx_id: tx.tx_id,
                writes: tx.visible_writes.clone(),
            })
            .collect()
    }

    pub fn all_committed(&self) -> Vec<TmRecoveredTx> {
        self.committed_through(u64::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmCommittedTx {
    pub tx_id: u64,
    pub visible_writes: Vec<TmWrite>,
    pub commit_end_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmRecoveredTx {
    pub tx_id: u64,
    pub writes: Vec<TmWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TmWrite {
    pub key: u64,
    pub value: u64,
    pub sub_xid: u64,
}

/// Drive the TM workload on the real filesystem.
pub fn run_tm_commit_path_workload(
    dir: &Path,
    seed: u64,
    scenario: TmCommitPathScenario,
) -> io::Result<TmCommitPathModel> {
    run_tm_commit_path_workload_on(&StdVfs, dir, seed, scenario)
}

/// Drive one commit-path scenario through the supplied durable-I/O backend.
pub fn run_tm_commit_path_workload_on<V: Vfs>(
    vfs: &V,
    dir: &Path,
    seed: u64,
    scenario: TmCommitPathScenario,
) -> io::Result<TmCommitPathModel> {
    let wal_path = dir.join(WAL_FILE_NAME);
    let mut wal = vfs.open(&wal_path, OpenMode::create_truncate())?;
    wal.write_all(&encode_wal_file_header())?;
    wal.sync_all()?;

    if scenario == TmCommitPathScenario::FcwBeforeWalAppend {
        return Ok(TmCommitPathModel {
            scenario,
            txs: Vec::new(),
            wal_len: WAL_FILE_HEADER_BYTES as u64,
        });
    }

    let mut rng = SplitMix64::new(seed ^ 0x544d_5f43_4d54_5f50); // "TM_CMT_P"
    let mut offset = WAL_FILE_HEADER_BYTES as u64;
    let txs = match scenario {
        TmCommitPathScenario::FcwBeforeWalAppend => unreachable!(),
        TmCommitPathScenario::WalAppendBeforeFinalize => {
            let tx_id = 1;
            let writes = vec![
                TmWrite {
                    key: 10,
                    value: 1 + rng.below(1000),
                    sub_xid: 0,
                },
                TmWrite {
                    key: 11,
                    value: 1 + rng.below(1000),
                    sub_xid: 0,
                },
            ];
            vec![append_tm_tx(&mut wal, &mut offset, tx_id, &writes, &[])?]
        }
        TmCommitPathScenario::SavepointReleaseRollback => {
            let tx_id = 1;
            let kept_sub = 101;
            let rolled_back_sub = 102;
            let all_writes = vec![
                TmWrite {
                    key: 20,
                    value: 1 + rng.below(1000),
                    sub_xid: 0,
                },
                TmWrite {
                    key: 21,
                    value: 1 + rng.below(1000),
                    sub_xid: kept_sub,
                },
                TmWrite {
                    key: 22,
                    value: 1 + rng.below(1000),
                    sub_xid: rolled_back_sub,
                },
            ];
            let control = [
                tm_action_release_sub(kept_sub),
                tm_action_rollback_sub(rolled_back_sub),
            ];
            vec![append_tm_tx(
                &mut wal,
                &mut offset,
                tx_id,
                &all_writes,
                &control,
            )?]
        }
        TmCommitPathScenario::ConcurrentWriters => {
            let tx1 = vec![TmWrite {
                key: 30,
                value: 1 + rng.below(1000),
                sub_xid: 0,
            }];
            let tx2 = vec![TmWrite {
                key: 31,
                value: 1 + rng.below(1000),
                sub_xid: 0,
            }];
            vec![
                append_tm_tx(&mut wal, &mut offset, 1, &tx1, &[])?,
                append_tm_tx(&mut wal, &mut offset, 2, &tx2, &[])?,
            ]
        }
    };

    Ok(TmCommitPathModel {
        scenario,
        txs,
        wal_len: offset,
    })
}

fn append_tm_tx<F: VfsFile>(
    wal: &mut F,
    offset: &mut u64,
    tx_id: u64,
    writes: &[TmWrite],
    control: &[Vec<u8>],
) -> io::Result<TmCommittedTx> {
    append_frame(wal, offset, &MainWalRecordFrame::Begin { tx_id })?;
    for write in writes {
        append_frame(
            wal,
            offset,
            &MainWalRecordFrame::PageWrite {
                tx_id,
                page_id: u32::try_from(write.key).unwrap_or(u32::MAX),
                data: tm_action_write(write),
            },
        )?;
    }
    for action in control {
        append_frame(
            wal,
            offset,
            &MainWalRecordFrame::TxCommitBatch {
                tx_id,
                actions: vec![action.clone()],
            },
        )?;
    }
    append_frame(wal, offset, &MainWalRecordFrame::Commit { tx_id })?;
    wal.sync_all()?;
    let commit_end_offset = *offset;

    let rolled_back: BTreeSet<u64> = control
        .iter()
        .filter_map(|action| parse_control_sub(action, b"rollback-sub:"))
        .collect();
    let visible_writes = writes
        .iter()
        .filter(|write| !rolled_back.contains(&write.sub_xid))
        .cloned()
        .collect();

    Ok(TmCommittedTx {
        tx_id,
        visible_writes,
        commit_end_offset,
    })
}

fn append_frame<F: VfsFile>(
    wal: &mut F,
    offset: &mut u64,
    frame: &MainWalRecordFrame,
) -> io::Result<()> {
    let bytes = encode_main_wal_record_frame(frame, WORKLOAD_TERM)?;
    wal.write_all(&bytes)?;
    *offset += bytes.len() as u64;
    Ok(())
}

fn tm_action_write(write: &TmWrite) -> Vec<u8> {
    format!("tm-write:{}:{}:{}", write.key, write.value, write.sub_xid).into_bytes()
}

fn tm_action_release_sub(sub_xid: u64) -> Vec<u8> {
    format!("release-sub:{sub_xid}").into_bytes()
}

fn tm_action_rollback_sub(sub_xid: u64) -> Vec<u8> {
    format!("rollback-sub:{sub_xid}").into_bytes()
}

fn parse_write(bytes: &[u8]) -> Option<TmWrite> {
    let raw = std::str::from_utf8(bytes).ok()?;
    let rest = raw.strip_prefix("tm-write:")?;
    let mut fields = rest.split(':');
    Some(TmWrite {
        key: fields.next()?.parse().ok()?,
        value: fields.next()?.parse().ok()?,
        sub_xid: fields.next()?.parse().ok()?,
    })
}

fn parse_control_sub(bytes: &[u8], prefix: &[u8]) -> Option<u64> {
    bytes
        .strip_prefix(prefix)
        .and_then(|raw| std::str::from_utf8(raw).ok())
        .and_then(|raw| raw.parse().ok())
}

/// Recover committed TM writes from the longest valid WAL prefix.
pub fn recover_tm_commit_path(dir: &Path) -> io::Result<Vec<TmRecoveredTx>> {
    let bytes = match std::fs::read(dir.join(WAL_FILE_NAME)) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err),
    };
    if bytes.len() < WAL_FILE_HEADER_BYTES {
        return Ok(Vec::new());
    }
    let mut header = [0u8; WAL_FILE_HEADER_BYTES];
    header.copy_from_slice(&bytes[..WAL_FILE_HEADER_BYTES]);
    let version = match decode_wal_file_header(&header) {
        Ok(header) => header.version,
        Err(_) => return Ok(Vec::new()),
    };

    let mut cursor = Cursor::new(bytes);
    cursor.set_position(WAL_FILE_HEADER_BYTES as u64);
    let mut open: BTreeMap<u64, TxBuilder> = BTreeMap::new();
    let mut committed = Vec::new();

    while let Ok(Some((_term, frame))) =
        decode_main_wal_record_frame(&mut cursor, version, WORKLOAD_TERM)
    {
        match frame {
            MainWalRecordFrame::Begin { tx_id } => {
                open.insert(tx_id, TxBuilder::default());
            }
            MainWalRecordFrame::PageWrite { tx_id, data, .. } => {
                if let Some(write) = parse_write(&data) {
                    open.entry(tx_id).or_default().writes.push(write);
                }
            }
            MainWalRecordFrame::TxCommitBatch { tx_id, actions } => {
                let tx = open.entry(tx_id).or_default();
                for action in actions {
                    if let Some(sub_xid) = parse_control_sub(&action, b"release-sub:") {
                        tx.released_sub_xids.insert(sub_xid);
                    }
                    if let Some(sub_xid) = parse_control_sub(&action, b"rollback-sub:") {
                        tx.rolled_back_sub_xids.insert(sub_xid);
                    }
                }
            }
            MainWalRecordFrame::Commit { tx_id } => {
                if let Some(tx) = open.remove(&tx_id) {
                    let writes = tx.visible_writes();
                    committed.push(TmRecoveredTx { tx_id, writes });
                }
            }
            MainWalRecordFrame::Rollback { tx_id } => {
                open.remove(&tx_id);
            }
            MainWalRecordFrame::Checkpoint { .. }
            | MainWalRecordFrame::FullPageImage { .. }
            | MainWalRecordFrame::VectorInsert { .. } => {}
        }
    }

    Ok(committed)
}

#[derive(Debug, Default)]
struct TxBuilder {
    writes: Vec<TmWrite>,
    released_sub_xids: BTreeSet<u64>,
    rolled_back_sub_xids: BTreeSet<u64>,
}

impl TxBuilder {
    fn visible_writes(self) -> Vec<TmWrite> {
        self.writes
            .into_iter()
            .filter(|write| {
                write.sub_xid == 0
                    || (self.released_sub_xids.contains(&write.sub_xid)
                        && !self.rolled_back_sub_xids.contains(&write.sub_xid))
            })
            .collect()
    }
}

/// Assert recovered state is one of the valid all-or-nothing commit outcomes
/// for this scenario/seed, and that savepoint sub-xids are not orphan-visible.
pub fn assert_tm_recovery_matches(
    seed: u64,
    model: &TmCommitPathModel,
    recovered: &[TmRecoveredTx],
) {
    let valid_states = valid_recovered_states(model);
    assert!(
        valid_states
            .iter()
            .any(|valid| valid.as_slice() == recovered),
        "SEED={seed} scenario={} recovered half-finalized TM state: {recovered:?}; \
         expected one of {valid_states:?}",
        model.scenario.name()
    );

    for tx in recovered {
        for write in &tx.writes {
            assert!(
                write.sub_xid == 0
                    || model
                        .txs
                        .iter()
                        .find(|expected_tx| expected_tx.tx_id == tx.tx_id)
                        .is_some_and(|expected_tx| expected_tx.visible_writes.contains(write)),
                "SEED={seed} scenario={} exposed orphan savepoint sub-xid {} in tx {}",
                model.scenario.name(),
                write.sub_xid,
                tx.tx_id
            );
        }
    }
}

fn valid_recovered_states(model: &TmCommitPathModel) -> Vec<Vec<TmRecoveredTx>> {
    if model.scenario == TmCommitPathScenario::ConcurrentWriters {
        let mut states = vec![Vec::new()];
        for tx in &model.txs {
            let mut next = states.last().cloned().unwrap_or_default();
            next.push(TmRecoveredTx {
                tx_id: tx.tx_id,
                writes: tx.visible_writes.clone(),
            });
            states.push(next);
        }
        states
    } else {
        let full = model.all_committed();
        if full.is_empty() {
            vec![Vec::new()]
        } else {
            vec![Vec::new(), full]
        }
    }
}
