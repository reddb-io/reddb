//! Blockchain collection kind — pure logic.
//!
//! A `KIND blockchain` collection stores append-only rows whose `hash` field
//! depends on the previous row's hash, forming a tamper-evident chain.
//! Issue #521 lands the engine integration in a later iteration; this module
//! ships the deterministic primitives (hash, verify_chain, error types) so the
//! later wiring is a thin storage adapter on top of audited logic.

use crate::crypto::Sha256;

/// All-zero hash used as `prev_hash` for the genesis block.
pub const GENESIS_PREV_HASH: [u8; 32] = [0u8; 32];

/// Optional signer fields included in the hash preimage when the collection
/// has `SIGNED_BY (...)` declared. Issue #520 supplies the signer registry;
/// the chain hash binds the signature so a replaced signature also breaks the
/// chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedFields {
    pub signer_pubkey: [u8; 32],
    pub signature: Vec<u8>,
}

/// A materialized block as stored. `hash` MUST equal
/// `compute_block_hash(...)` over the other fields — `verify_chain` enforces
/// this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub block_height: u64,
    pub prev_hash: [u8; 32],
    pub timestamp_ms: u64,
    pub payload: Vec<u8>,
    pub signed: Option<SignedFields>,
    pub hash: [u8; 32],
}

/// Engine response to `GET /collections/:name/chain-tip`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainTip {
    pub block_height: u64,
    pub hash: [u8; 32],
    pub timestamp_ms: u64,
}

/// Operational errors surfaced by the blockchain engine path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockchainError {
    /// Client submitted a `prev_hash` that no longer matches the current tip
    /// (someone else appended first). Surface as HTTP 409.
    ConflictRetry {
        expected: [u8; 32],
        got: [u8; 32],
    },
    /// Caller attempted UPDATE or DELETE on a `KIND blockchain` collection.
    /// Surface as HTTP 409 (`BlockchainCollectionImmutable`).
    Immutable,
}

impl std::fmt::Display for BlockchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConflictRetry { .. } => f.write_str("BlockchainConflictRetry"),
            Self::Immutable => f.write_str("BlockchainCollectionImmutable"),
        }
    }
}

impl std::error::Error for BlockchainError {}

/// Result of walking a chain end-to-end. `Inconsistent` reports the FIRST
/// block whose stored fields disagree with a recomputed hash; the chain is
/// not walked past the first failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyReport {
    Ok,
    Inconsistent { block_height: u64, reason: String },
}

/// Canonical hash preimage:
///
///   prev_hash (32)
///   || block_height (u64 big-endian)
///   || timestamp_ms (u64 big-endian)
///   || payload_len   (u64 big-endian)
///   || payload bytes
///   || [if signed]
///        signer_pubkey (32)
///        || sig_len   (u64 big-endian)
///        || signature bytes
///
/// Length prefixes make the encoding unambiguous: a signed block with empty
/// payload cannot collide with an unsigned block whose payload happens to
/// equal the signer/signature concatenation.
pub fn compute_block_hash(
    prev_hash: &[u8; 32],
    block_height: u64,
    timestamp_ms: u64,
    payload: &[u8],
    signed: Option<&SignedFields>,
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(prev_hash);
    h.update(&block_height.to_be_bytes());
    h.update(&timestamp_ms.to_be_bytes());
    h.update(&(payload.len() as u64).to_be_bytes());
    h.update(payload);
    if let Some(s) = signed {
        h.update(&s.signer_pubkey);
        h.update(&(s.signature.len() as u64).to_be_bytes());
        h.update(&s.signature);
    }
    h.finalize()
}

/// Walk `blocks` in order. Returns the first inconsistency or `Ok` if every
/// block's stored hash matches the recomputed hash AND links the previous
/// block.
pub fn verify_chain(blocks: &[Block]) -> VerifyReport {
    let mut expected_prev: [u8; 32] = GENESIS_PREV_HASH;
    let mut expected_height: u64 = 0;
    for block in blocks {
        if block.block_height != expected_height {
            return VerifyReport::Inconsistent {
                block_height: block.block_height,
                reason: format!(
                    "block_height mismatch: expected {expected_height}, got {}",
                    block.block_height
                ),
            };
        }
        if block.prev_hash != expected_prev {
            return VerifyReport::Inconsistent {
                block_height: block.block_height,
                reason: "prev_hash does not link previous block".to_string(),
            };
        }
        let recomputed = compute_block_hash(
            &block.prev_hash,
            block.block_height,
            block.timestamp_ms,
            &block.payload,
            block.signed.as_ref(),
        );
        if recomputed != block.hash {
            return VerifyReport::Inconsistent {
                block_height: block.block_height,
                reason: "stored hash does not match recomputed hash".to_string(),
            };
        }
        expected_prev = block.hash;
        expected_height = block.block_height.saturating_add(1);
    }
    VerifyReport::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_block(height: u64, prev: [u8; 32], payload: &[u8]) -> Block {
        let ts = 1_700_000_000_000 + height;
        let hash = compute_block_hash(&prev, height, ts, payload, None);
        Block {
            block_height: height,
            prev_hash: prev,
            timestamp_ms: ts,
            payload: payload.to_vec(),
            signed: None,
            hash,
        }
    }

    fn build_chain(n: u64) -> Vec<Block> {
        let mut out = Vec::new();
        let mut prev = GENESIS_PREV_HASH;
        for i in 0..n {
            let payload = format!("payload-{i}");
            let b = make_block(i, prev, payload.as_bytes());
            prev = b.hash;
            out.push(b);
        }
        out
    }

    #[test]
    fn genesis_prev_hash_is_zero() {
        assert_eq!(GENESIS_PREV_HASH, [0u8; 32]);
    }

    #[test]
    fn five_block_chain_verifies_ok() {
        let chain = build_chain(5);
        assert_eq!(verify_chain(&chain), VerifyReport::Ok);
        assert_eq!(chain[0].block_height, 0);
        assert_eq!(chain[0].prev_hash, GENESIS_PREV_HASH);
        assert_eq!(chain[4].block_height, 4);
    }

    #[test]
    fn corrupting_block_two_payload_is_reported() {
        let mut chain = build_chain(5);
        chain[2].payload = b"tampered".to_vec();
        match verify_chain(&chain) {
            VerifyReport::Inconsistent { block_height, .. } => {
                assert_eq!(block_height, 2);
            }
            VerifyReport::Ok => panic!("tampered chain reported Ok"),
        }
    }

    #[test]
    fn corrupting_prev_hash_breaks_chain() {
        let mut chain = build_chain(3);
        chain[1].prev_hash = [0xAAu8; 32];
        // Recompute hash so the per-block hash check passes; the linkage
        // check must still fail.
        chain[1].hash = compute_block_hash(
            &chain[1].prev_hash,
            chain[1].block_height,
            chain[1].timestamp_ms,
            &chain[1].payload,
            None,
        );
        match verify_chain(&chain) {
            VerifyReport::Inconsistent { block_height, reason } => {
                assert_eq!(block_height, 1);
                assert!(reason.contains("prev_hash"));
            }
            VerifyReport::Ok => panic!("broken linkage reported Ok"),
        }
    }

    #[test]
    fn signed_field_inclusion_changes_hash() {
        let prev = GENESIS_PREV_HASH;
        let payload = b"x";
        let unsigned = compute_block_hash(&prev, 0, 1, payload, None);
        let signed = compute_block_hash(
            &prev,
            0,
            1,
            payload,
            Some(&SignedFields {
                signer_pubkey: [0x11; 32],
                signature: vec![0x22; 64],
            }),
        );
        assert_ne!(unsigned, signed);
    }

    #[test]
    fn empty_payload_signed_vs_unsigned_disambiguates() {
        // Length-prefix encoding must prevent a signed block from colliding
        // with an unsigned block that has the signer bytes inlined in payload.
        let prev = GENESIS_PREV_HASH;
        let signer = [0x55u8; 32];
        let sig = vec![0x66u8; 8];
        let signed = compute_block_hash(
            &prev,
            7,
            42,
            b"",
            Some(&SignedFields {
                signer_pubkey: signer,
                signature: sig.clone(),
            }),
        );
        let mut spoof_payload = Vec::new();
        spoof_payload.extend_from_slice(&signer);
        spoof_payload.extend_from_slice(&(sig.len() as u64).to_be_bytes());
        spoof_payload.extend_from_slice(&sig);
        let unsigned = compute_block_hash(&prev, 7, 42, &spoof_payload, None);
        assert_ne!(signed, unsigned);
    }

    #[test]
    fn conflict_retry_display() {
        let err = BlockchainError::ConflictRetry {
            expected: [1u8; 32],
            got: [2u8; 32],
        };
        assert_eq!(err.to_string(), "BlockchainConflictRetry");
        assert_eq!(
            BlockchainError::Immutable.to_string(),
            "BlockchainCollectionImmutable"
        );
    }
}
