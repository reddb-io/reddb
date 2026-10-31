//! Foundation for `KIND blockchain` collections (issue #523).
//!
//! Persists kind in `red_config`, exposes the reserved-column set used by
//! every block row, scans the collection to derive the current chain tip,
//! and wraps the hash helpers from [`crate::storage::blockchain`].
//!
//! This iteration does NOT validate user-supplied prev_hash/height on INSERT
//! (the engine computes them) and does NOT enforce conflict-retry semantics
//! — those land alongside chain-tip RPC and `verify_chain` in a later slice.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::blockchain::{
    compute_block_hash, verify_chain, Block, VerifyReport, GENESIS_PREV_HASH,
};
use crate::storage::unified::{EntityData, RowData, SegmentManager, UnifiedStore};
use crate::{RedDBError, RedDBResult};
use reddb_types::Value;

/// Value stored under `red.collection.{name}.kind` for blockchain collections.
pub const CHAIN_KIND_TAG: &str = "chain";

pub const COL_BLOCK_HEIGHT: &str = "block_height";
pub const COL_PREV_HASH: &str = "prev_hash";
pub const COL_TIMESTAMP: &str = "timestamp";
pub const COL_HASH: &str = "hash";

/// Reserved column names auto-filled by the engine; user INSERTs that supply
/// them are silently overwritten so the chain remains engine-controlled.
pub const RESERVED_COLUMNS: &[&str] = &[COL_BLOCK_HEIGHT, COL_PREV_HASH, COL_TIMESTAMP, COL_HASH];

/// Chain tip derived by scanning the collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainTip {
    /// `block_height` of the highest block. `None` if the collection has no
    /// rows yet (pre-genesis; callers should use [`GENESIS_PREV_HASH`] for
    /// `prev_hash` and `0` for `block_height`).
    pub height: Option<u64>,
    pub hash: [u8; 32],
}

impl ChainTip {
    pub fn empty() -> Self {
        Self {
            height: None,
            hash: GENESIS_PREV_HASH,
        }
    }

    /// Returns `(prev_hash, next_height)` to use when appending a new block.
    pub fn next(&self) -> ([u8; 32], u64) {
        let next_height = self.height.map(|h| h + 1).unwrap_or(0);
        (self.hash, next_height)
    }
}

fn kind_key(collection: &str) -> String {
    format!("red.collection.{collection}.kind")
}

/// Legacy best-effort adapter. Authoritative creation uses [`try_mark_as_chain`].
pub fn mark_as_chain(store: &UnifiedStore, collection: &str) {
    let _ = try_mark_as_chain(store, collection);
}

pub fn try_mark_as_chain(store: &UnifiedStore, collection: &str) -> RedDBResult<()> {
    store.try_set_config_tree(
        &kind_key(collection),
        &crate::serde_json::Value::String(CHAIN_KIND_TAG.to_string()),
    )?;
    Ok(())
}

/// Compatibility probe fails closed; authoritative callers propagate errors.
pub fn is_chain(store: &UnifiedStore, collection: &str) -> bool {
    try_is_chain(store, collection).unwrap_or(true)
}

pub fn try_is_chain(store: &UnifiedStore, collection: &str) -> RedDBResult<bool> {
    match store.try_get_config(&kind_key(collection))? {
        None => Ok(false),
        Some(Value::Text(kind)) => Ok(kind.as_ref() == CHAIN_KIND_TAG),
        Some(_) => Err(RedDBError::Query(
            "invalid persisted collection kind".into(),
        )),
    }
}

/// Full chain tip including timestamp. Returned by `chain_tip_full` and
/// `GET /collections/:name/chain-tip`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainTipFull {
    pub height: u64,
    pub hash: [u8; 32],
    pub timestamp_ms: u64,
}

/// Legacy optional read. Authoritative append/HTTP paths use the fallible API.
pub fn chain_tip_full(store: &UnifiedStore, collection: &str) -> Option<ChainTipFull> {
    try_chain_tip_full(store, collection).ok().flatten()
}

pub fn try_chain_tip_full(
    store: &UnifiedStore,
    collection: &str,
) -> RedDBResult<Option<ChainTipFull>> {
    let Some(manager) = store.get_collection(collection) else {
        return Ok(None);
    };
    let mut best: Option<ChainTipFull> = None;
    visit_chain_rows(&manager, |row| {
        let tip = chain_row_tip(row)?;
        if best
            .as_ref()
            .is_none_or(|current| tip.height > current.height)
        {
            best = Some(tip);
        }
        Ok(())
    })?;
    Ok(best)
}

/// Legacy convenience adapter; creation uses [`try_chain_tip`].
pub fn chain_tip(store: &UnifiedStore, collection: &str) -> ChainTip {
    try_chain_tip(store, collection).unwrap_or_else(|_| ChainTip::empty())
}

pub fn try_chain_tip(store: &UnifiedStore, collection: &str) -> RedDBResult<ChainTip> {
    Ok(match try_chain_tip_full(store, collection)? {
        Some(tip) => ChainTip {
            height: Some(tip.height),
            hash: tip.hash,
        },
        None => ChainTip::empty(),
    })
}

fn visit_chain_rows(
    manager: &SegmentManager,
    mut consume: impl FnMut(&RowData) -> RedDBResult<()>,
) -> RedDBResult<()> {
    let mut failure = None;
    super::function_budget::try_scan(
        |visit| manager.try_for_each_entity(usize::MAX, visit),
        |entity| {
            let result = match &entity.data {
                EntityData::Row(row) => consume(row),
                _ => Err(RedDBError::Query(
                    "blockchain contains a non-row record".into(),
                )),
            };
            match result {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        },
    )?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn chain_row_u64(row: &RowData, field: &str) -> RedDBResult<u64> {
    match row.get_field(field) {
        Some(Value::UnsignedInteger(value)) => Ok(*value),
        Some(Value::Integer(value)) if *value >= 0 => Ok(*value as u64),
        _ => Err(RedDBError::Query(format!(
            "invalid blockchain field {field}"
        ))),
    }
}

fn chain_row_hash(row: &RowData, field: &str) -> RedDBResult<[u8; 32]> {
    match row.get_field(field) {
        Some(Value::Blob(bytes)) => bytes
            .as_slice()
            .try_into()
            .map_err(|_| RedDBError::Query(format!("invalid blockchain field {field}"))),
        _ => Err(RedDBError::Query(format!(
            "invalid blockchain field {field}"
        ))),
    }
}

fn chain_row_tip(row: &RowData) -> RedDBResult<ChainTipFull> {
    Ok(ChainTipFull {
        height: chain_row_u64(row, COL_BLOCK_HEIGHT)?,
        hash: chain_row_hash(row, COL_HASH)?,
        timestamp_ms: chain_row_u64(row, COL_TIMESTAMP)?,
    })
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Build the `BlockchainConflict:<json>` error payload mapped to HTTP 409
/// (#524). The JSON body carries the current tip so the caller can retry
/// with the right `prev_hash` / `block_height`.
pub fn chain_conflict_error(
    tip_height: u64,
    tip_hash: [u8; 32],
    tip_timestamp_ms: u64,
    server_now_ms: u64,
    reason: &str,
) -> crate::api::RedDBError {
    let body = format!(
        "{{\"block_height\":{},\"hash\":\"{}\",\"timestamp\":{},\"server_time\":{},\"reason\":\"{}\"}}",
        tip_height,
        hex32(&tip_hash),
        tip_timestamp_ms,
        server_now_ms,
        reason.replace('"', "'")
    );
    crate::api::RedDBError::InvalidOperation(format!("BlockchainConflict:{body}"))
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Canonicalize user-supplied fields for inclusion in the block hash. Sorted
/// by key, `<key>=<plain_text>;` joined — stable across reorderings so the
/// recomputed hash matches regardless of column order at INSERT-time.
pub fn canonical_payload(fields: &[(String, Value)]) -> Vec<u8> {
    let mut pairs: Vec<(&str, String)> = fields
        .iter()
        .filter(|(k, _)| !RESERVED_COLUMNS.contains(&k.as_str()))
        .map(|(k, v)| (k.as_str(), v.plain_text()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = Vec::new();
    for (k, v) in pairs {
        out.extend_from_slice(k.as_bytes());
        out.push(b'=');
        out.extend_from_slice(v.as_bytes());
        out.push(b';');
    }
    out
}

/// Build the reserved-column key/value pairs for a new block. Caller appends
/// these to the row's `fields` AFTER stripping any user-supplied reserved
/// columns. The returned `hash` is also returned so callers can advance the
/// tip without recomputing.
pub fn make_block_reserved_fields(
    prev_hash: [u8; 32],
    height: u64,
    timestamp_ms: u64,
    payload_canonical: &[u8],
) -> (Vec<(String, Value)>, [u8; 32]) {
    let hash = compute_block_hash(&prev_hash, height, timestamp_ms, payload_canonical, None);
    let fields = vec![
        (COL_BLOCK_HEIGHT.to_string(), Value::UnsignedInteger(height)),
        (COL_PREV_HASH.to_string(), Value::Blob(prev_hash.to_vec())),
        (
            COL_TIMESTAMP.to_string(),
            Value::UnsignedInteger(timestamp_ms),
        ),
        (COL_HASH.to_string(), Value::Blob(hash.to_vec())),
    ];
    (fields, hash)
}

/// Issue #525 — walk a chain collection in `block_height` order and rebuild the
/// `Block` list expected by [`verify_chain`]. Genesis is identified by
/// `block_height == 0`.  Returns `None` when the collection is absent or not a
/// chain.  Reserved-column extraction and canonical payload encoding mirror
/// what the engine writes at INSERT time — this is the alignment of the two
/// encoders referenced in #524.
pub fn collect_blocks(store: &UnifiedStore, collection: &str) -> Option<Vec<Block>> {
    try_collect_blocks(store, collection).ok().flatten()
}

pub fn try_collect_blocks(
    store: &UnifiedStore,
    collection: &str,
) -> RedDBResult<Option<Vec<Block>>> {
    if !try_is_chain(store, collection)? {
        return Ok(None);
    }
    let Some(manager) = store.get_collection(collection) else {
        return Ok(None);
    };
    let mut blocks = Vec::new();
    visit_chain_rows(&manager, |row| {
        let tip = chain_row_tip(row)?;
        let prev_hash = chain_row_hash(row, COL_PREV_HASH)?;
        let user_fields: Vec<_> = row
            .iter_fields()
            .filter(|(name, _)| !RESERVED_COLUMNS.contains(name))
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect();
        blocks.push(Block {
            block_height: tip.height,
            prev_hash,
            timestamp_ms: tip.timestamp_ms,
            payload: canonical_payload(&user_fields),
            signed: None,
            hash: tip.hash,
        });
        Ok(())
    })?;
    blocks.sort_by_key(|block| block.block_height);
    Ok(Some(blocks))
}

/// Outcome of `POST /collections/:name/verify-chain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyChainOutcome {
    pub checked: u64,
    pub ok: bool,
    pub first_bad_height: Option<u64>,
}

/// Walk the chain end-to-end. Returns `(outcome, _)`.  Mismatches are detected
/// via [`verify_chain`].  Callers persist the integrity flag on `ok == false`.
pub fn verify_chain_outcome(store: &UnifiedStore, collection: &str) -> Option<VerifyChainOutcome> {
    try_verify_chain_outcome(store, collection).ok().flatten()
}

pub fn try_verify_chain_outcome(
    store: &UnifiedStore,
    collection: &str,
) -> RedDBResult<Option<VerifyChainOutcome>> {
    let Some(blocks) = try_collect_blocks(store, collection)? else {
        return Ok(None);
    };
    if blocks.is_empty() {
        return Err(RedDBError::Query("blockchain has no genesis block".into()));
    }
    let checked = blocks.len() as u64;
    Ok(Some(match verify_chain(&blocks) {
        VerifyReport::Ok => VerifyChainOutcome {
            checked,
            ok: true,
            first_bad_height: None,
        },
        VerifyReport::Inconsistent { block_height, .. } => VerifyChainOutcome {
            checked,
            ok: false,
            first_bad_height: Some(block_height),
        },
    }))
}

fn integrity_key(collection: &str) -> String {
    format!("red.collection.{collection}.integrity")
}

const INTEGRITY_BROKEN: &str = "broken";
const INTEGRITY_OK: &str = "ok";

/// Legacy best-effort adapter; runtime verification uses the fallible API.
pub fn persist_integrity_flag(store: &UnifiedStore, collection: &str, broken: bool) {
    let _ = try_persist_integrity_flag(store, collection, broken);
}

pub fn try_persist_integrity_flag(
    store: &UnifiedStore,
    collection: &str,
    broken: bool,
) -> RedDBResult<()> {
    let tag = if broken {
        INTEGRITY_BROKEN
    } else {
        INTEGRITY_OK
    };
    store.try_set_config_tree(
        &integrity_key(collection),
        &crate::serde_json::Value::String(tag.to_string()),
    )?;
    Ok(())
}

pub fn is_integrity_broken_persisted(store: &UnifiedStore, collection: &str) -> Option<bool> {
    try_is_integrity_broken_persisted(store, collection)
        .ok()
        .flatten()
}

pub fn try_is_integrity_broken_persisted(
    store: &UnifiedStore,
    collection: &str,
) -> RedDBResult<Option<bool>> {
    match store.try_get_config(&integrity_key(collection))? {
        None => Ok(None),
        Some(Value::Text(tag)) if tag.as_ref() == INTEGRITY_BROKEN => Ok(Some(true)),
        Some(Value::Text(tag)) if tag.as_ref() == INTEGRITY_OK => Ok(Some(false)),
        Some(_) => Err(RedDBError::Query(
            "invalid persisted blockchain integrity flag".into(),
        )),
    }
}

/// Convenience: produce the genesis row's full field list. Genesis carries
/// an empty user payload — extra metadata is recorded in subsequent blocks.
pub fn genesis_fields(timestamp_ms: u64) -> Vec<(String, Value)> {
    make_block_reserved_fields(GENESIS_PREV_HASH, 0, timestamp_ms, &[]).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_columns_complete() {
        assert_eq!(RESERVED_COLUMNS.len(), 4);
        assert!(RESERVED_COLUMNS.contains(&COL_BLOCK_HEIGHT));
        assert!(RESERVED_COLUMNS.contains(&COL_PREV_HASH));
        assert!(RESERVED_COLUMNS.contains(&COL_TIMESTAMP));
        assert!(RESERVED_COLUMNS.contains(&COL_HASH));
    }

    #[test]
    fn empty_tip_advances_to_genesis_height() {
        let tip = ChainTip::empty();
        let (prev, height) = tip.next();
        assert_eq!(prev, GENESIS_PREV_HASH);
        assert_eq!(height, 0);
    }

    #[test]
    fn tip_with_height_advances_by_one() {
        let tip = ChainTip {
            height: Some(7),
            hash: [0xAB; 32],
        };
        let (prev, height) = tip.next();
        assert_eq!(prev, [0xAB; 32]);
        assert_eq!(height, 8);
    }

    #[test]
    fn genesis_fields_carry_zero_prev_hash() {
        let fields = genesis_fields(1_700_000_000_000);
        let prev = fields.iter().find(|(k, _)| k == COL_PREV_HASH).unwrap();
        match &prev.1 {
            Value::Blob(b) => assert_eq!(&b[..], &[0u8; 32]),
            _ => panic!("prev_hash must be Blob"),
        }
        let height = fields.iter().find(|(k, _)| k == COL_BLOCK_HEIGHT).unwrap();
        assert_eq!(height.1, Value::UnsignedInteger(0));
    }

    #[test]
    fn canonical_payload_is_order_independent() {
        let a = vec![
            ("user".to_string(), Value::text("alice")),
            ("amount".to_string(), Value::Integer(100)),
        ];
        let b = vec![
            ("amount".to_string(), Value::Integer(100)),
            ("user".to_string(), Value::text("alice")),
        ];
        assert_eq!(canonical_payload(&a), canonical_payload(&b));
    }

    #[test]
    fn canonical_payload_skips_reserved_columns() {
        let fields = vec![
            ("user".to_string(), Value::text("alice")),
            (COL_BLOCK_HEIGHT.to_string(), Value::UnsignedInteger(42)),
            (COL_HASH.to_string(), Value::Blob(vec![0xFF; 32])),
        ];
        let bytes = canonical_payload(&fields);
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "user=alice;");
    }

    #[test]
    fn block_hash_matches_recompute() {
        let (fields, hash) =
            make_block_reserved_fields(GENESIS_PREV_HASH, 0, 1_700_000_000_000, b"user=alice;");
        let recomputed = compute_block_hash(
            &GENESIS_PREV_HASH,
            0,
            1_700_000_000_000,
            b"user=alice;",
            None,
        );
        assert_eq!(hash, recomputed);
        let stored = fields.iter().find(|(k, _)| k == COL_HASH).unwrap();
        match &stored.1 {
            Value::Blob(b) => assert_eq!(&b[..], &hash[..]),
            _ => panic!("hash must be Blob"),
        }
    }
}
