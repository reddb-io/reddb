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

use crate::storage::blockchain::{compute_block_hash, GENESIS_PREV_HASH};
use crate::storage::schema::Value;
use crate::storage::unified::UnifiedStore;

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

/// Persist the `chain` kind marker. Append-only — only call once at creation.
pub fn mark_as_chain(store: &UnifiedStore, collection: &str) {
    store.set_config_tree(
        &kind_key(collection),
        &crate::serde_json::Value::String(CHAIN_KIND_TAG.to_string()),
    );
}

/// True if `mark_as_chain` was ever called for this collection.
pub fn is_chain(store: &UnifiedStore, collection: &str) -> bool {
    match store.get_config(&kind_key(collection)) {
        Some(Value::Text(s)) => s.as_ref() == CHAIN_KIND_TAG,
        _ => false,
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

/// Scan-based tip with timestamp. `None` when the collection has no rows
/// (pre-genesis). Used by the chain-tip endpoint and the chain-INSERT
/// validation path (#524).
pub fn chain_tip_full(store: &UnifiedStore, collection: &str) -> Option<ChainTipFull> {
    let manager = store.get_collection(collection)?;
    let mut best: Option<ChainTipFull> = None;
    for entity in manager.query_all(|_| true) {
        let crate::storage::unified::EntityData::Row(row) = &entity.data else {
            continue;
        };
        let Some(named) = &row.named else { continue };
        let height = match named.get(COL_BLOCK_HEIGHT) {
            Some(Value::UnsignedInteger(v)) => *v,
            Some(Value::Integer(v)) if *v >= 0 => *v as u64,
            _ => continue,
        };
        let hash = match named.get(COL_HASH) {
            Some(Value::Blob(b)) if b.len() == 32 => {
                let mut out = [0u8; 32];
                out.copy_from_slice(b);
                out
            }
            _ => continue,
        };
        let timestamp_ms = match named.get(COL_TIMESTAMP) {
            Some(Value::UnsignedInteger(v)) => *v,
            Some(Value::Integer(v)) if *v >= 0 => *v as u64,
            _ => 0,
        };
        match &best {
            None => {
                best = Some(ChainTipFull {
                    height,
                    hash,
                    timestamp_ms,
                });
            }
            Some(cur) if height > cur.height => {
                best = Some(ChainTipFull {
                    height,
                    hash,
                    timestamp_ms,
                });
            }
            _ => {}
        }
    }
    best
}

/// Scan the collection for the highest `block_height` and return its row's
/// `hash`. O(n) — replaced by a cached tip in a later iteration.
pub fn chain_tip(store: &UnifiedStore, collection: &str) -> ChainTip {
    let Some(manager) = store.get_collection(collection) else {
        return ChainTip::empty();
    };
    let mut best: Option<(u64, [u8; 32])> = None;
    for entity in manager.query_all(|_| true) {
        let crate::storage::unified::EntityData::Row(row) = &entity.data else {
            continue;
        };
        let Some(named) = &row.named else {
            continue;
        };
        let height = match named.get(COL_BLOCK_HEIGHT) {
            Some(Value::UnsignedInteger(v)) => *v,
            Some(Value::Integer(v)) if *v >= 0 => *v as u64,
            _ => continue,
        };
        let hash = match named.get(COL_HASH) {
            Some(Value::Blob(b)) if b.len() == 32 => {
                let mut out = [0u8; 32];
                out.copy_from_slice(b);
                out
            }
            _ => continue,
        };
        match best {
            None => best = Some((height, hash)),
            Some((h, _)) if height > h => best = Some((height, hash)),
            _ => {}
        }
    }
    match best {
        Some((height, hash)) => ChainTip {
            height: Some(height),
            hash,
        },
        None => ChainTip::empty(),
    }
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
        (
            COL_BLOCK_HEIGHT.to_string(),
            Value::UnsignedInteger(height),
        ),
        (COL_PREV_HASH.to_string(), Value::Blob(prev_hash.to_vec())),
        (
            COL_TIMESTAMP.to_string(),
            Value::UnsignedInteger(timestamp_ms),
        ),
        (COL_HASH.to_string(), Value::Blob(hash.to_vec())),
    ];
    (fields, hash)
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
            (
                COL_BLOCK_HEIGHT.to_string(),
                Value::UnsignedInteger(42),
            ),
            (COL_HASH.to_string(), Value::Blob(vec![0xFF; 32])),
        ];
        let bytes = canonical_payload(&fields);
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "user=alice;");
    }

    #[test]
    fn block_hash_matches_recompute() {
        let (fields, hash) = make_block_reserved_fields(
            GENESIS_PREV_HASH,
            0,
            1_700_000_000_000,
            b"user=alice;",
        );
        let recomputed =
            compute_block_hash(&GENESIS_PREV_HASH, 0, 1_700_000_000_000, b"user=alice;", None);
        assert_eq!(hash, recomputed);
        let stored = fields.iter().find(|(k, _)| k == COL_HASH).unwrap();
        match &stored.1 {
            Value::Blob(b) => assert_eq!(&b[..], &hash[..]),
            _ => panic!("hash must be Blob"),
        }
    }
}
