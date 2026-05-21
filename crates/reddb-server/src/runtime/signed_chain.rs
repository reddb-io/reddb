//! Issue #526 — composition of `KIND blockchain` + `SIGNED_BY (...)`.
//!
//! Locks the contract a `KIND blockchain SIGNED_BY (...)` collection ships:
//!
//! * The block hash binds the chain fields AND the row's signer pubkey +
//!   signature. Tampering with either reserved column breaks `verify_chain`
//!   at that height — the hash is now a function of `(prev_hash,
//!   block_height, timestamp, canonical(payload), signer_pubkey,
//!   signature)`.
//! * Genesis is exempt: `block_height == 0` carries the all-zero pubkey
//!   and an empty signature so the collection can be created before any
//!   signer registers a row. Every subsequent block MUST carry a
//!   non-genesis (allowed-signer) signature.
//! * `verify_chain_with_signatures` walks the chain and additionally
//!   re-verifies the Ed25519 signature on each non-genesis block, so an
//!   integrity scan flags signature tampering even when the stored
//!   `hash` was recomputed to "match" the tampered bytes.
//!
//! This module is pure logic on top of the audited primitives in
//! [`storage::blockchain`](crate::storage::blockchain) and
//! [`storage::signed_writes`](crate::storage::signed_writes). Runtime
//! wiring (INSERT pipeline composition, DDL persistence of the registry
//! on a `KIND blockchain` collection, REST error mapping) is owned by
//! the parent issues #522 and #524 and is consumed by this module via
//! the same primitives once both land.

use crate::storage::blockchain::{
    compute_block_hash, verify_chain, Block, SignedFields, VerifyReport, GENESIS_PREV_HASH,
};
use crate::storage::schema::Value;
use crate::storage::signed_writes::{
    reverify_row, RESERVED_SIGNATURE_COL, RESERVED_SIGNER_PUBKEY_COL, SIGNATURE_LEN,
    SIGNER_PUBKEY_LEN,
};

use super::blockchain_kind::{COL_BLOCK_HEIGHT, COL_HASH, COL_PREV_HASH, COL_TIMESTAMP};

/// All-zero pubkey marker recorded on the genesis row of a signed chain.
/// Documented exemption: the genesis block predates any signer's first
/// `INSERT` so it cannot itself carry a real signature.
pub const GENESIS_SIGNER_PUBKEY: [u8; SIGNER_PUBKEY_LEN] = [0u8; SIGNER_PUBKEY_LEN];

/// Empty signature recorded on the genesis row. Pair with
/// [`GENESIS_SIGNER_PUBKEY`].
pub const GENESIS_SIGNATURE: [u8; SIGNATURE_LEN] = [0u8; SIGNATURE_LEN];

/// Reserved column set for a `KIND blockchain SIGNED_BY (...)` collection
/// — the union of the chain reserved columns and the signed-writes
/// reserved columns.
pub const RESERVED_COLUMNS_SIGNED_CHAIN: &[&str] = &[
    COL_BLOCK_HEIGHT,
    COL_PREV_HASH,
    COL_TIMESTAMP,
    COL_HASH,
    RESERVED_SIGNER_PUBKEY_COL,
    RESERVED_SIGNATURE_COL,
];

/// True for the documented genesis exemption pair (null pubkey + null
/// signature). Used by the verify walker to skip Ed25519 verification on
/// the genesis row.
pub fn is_genesis_signed_marker(pubkey: &[u8; SIGNER_PUBKEY_LEN], signature: &[u8]) -> bool {
    pubkey == &GENESIS_SIGNER_PUBKEY && signature.iter().all(|b| *b == 0)
}

/// Build the reserved-column field list + hash for a new block on a
/// signed chain. Caller supplies the row's canonical payload bytes
/// (engine's canonical payload encoder, identical to what the client
/// signed) and the signer fields produced by the client.
///
/// Genesis exemption: when `height == 0`, the caller passes
/// [`GENESIS_SIGNER_PUBKEY`] / [`GENESIS_SIGNATURE`] markers.
///
/// The returned hash binds the signer fields per
/// [`compute_block_hash`], so any subsequent tampering with either
/// reserved column makes the per-block hash check fail in
/// `verify_chain`.
pub fn make_signed_block_reserved_fields(
    prev_hash: [u8; 32],
    height: u64,
    timestamp_ms: u64,
    payload_canonical: &[u8],
    signer_pubkey: [u8; SIGNER_PUBKEY_LEN],
    signature: Vec<u8>,
) -> (Vec<(String, Value)>, [u8; 32]) {
    let signed = SignedFields {
        signer_pubkey,
        signature: signature.clone(),
    };
    let hash = compute_block_hash(
        &prev_hash,
        height,
        timestamp_ms,
        payload_canonical,
        Some(&signed),
    );
    let fields = vec![
        (COL_BLOCK_HEIGHT.to_string(), Value::UnsignedInteger(height)),
        (COL_PREV_HASH.to_string(), Value::Blob(prev_hash.to_vec())),
        (
            COL_TIMESTAMP.to_string(),
            Value::UnsignedInteger(timestamp_ms),
        ),
        (
            RESERVED_SIGNER_PUBKEY_COL.to_string(),
            Value::Blob(signer_pubkey.to_vec()),
        ),
        (RESERVED_SIGNATURE_COL.to_string(), Value::Blob(signature)),
        (COL_HASH.to_string(), Value::Blob(hash.to_vec())),
    ];
    (fields, hash)
}

/// Genesis row builder for a signed chain. Returns the field list that
/// `execute_create_collection` writes when the collection has both
/// `KIND blockchain` and a non-empty signer registry.
pub fn genesis_signed_fields(timestamp_ms: u64) -> Vec<(String, Value)> {
    make_signed_block_reserved_fields(
        GENESIS_PREV_HASH,
        0,
        timestamp_ms,
        &[],
        GENESIS_SIGNER_PUBKEY,
        GENESIS_SIGNATURE.to_vec(),
    )
    .0
}

/// Outcome of [`verify_chain_with_signatures`]. Distinguishes "hash chain
/// is broken" (recomputed hash differs from stored hash) from "signature
/// is invalid" (hash chain still links but the stored signature does
/// NOT verify against the stored pubkey over the canonical payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedChainVerifyOutcome {
    pub checked: u64,
    pub ok: bool,
    pub first_bad_height: Option<u64>,
    /// `true` when the failure was the per-block Ed25519 signature
    /// re-verification rather than the chain hash linkage.
    pub signature_failure: bool,
}

impl SignedChainVerifyOutcome {
    pub fn ok(checked: u64) -> Self {
        Self {
            checked,
            ok: true,
            first_bad_height: None,
            signature_failure: false,
        }
    }
}

/// Issue #526 — walk a signed chain end-to-end. Combines:
///
/// 1. [`verify_chain`] — hash chain linkage + per-block hash recompute
///    (already covers tampered signer/signature because they feed into
///    the hash preimage).
/// 2. Per-non-genesis-block Ed25519 signature re-verification via
///    [`reverify_row`]. This catches the pathological case where a
///    tamperer replaces stored `hash` to match a forged payload — the
///    chain links, the hash matches, but the signature does NOT verify
///    against the stored pubkey.
///
/// Genesis exemption: a block at `block_height == 0` with the documented
/// null pubkey + empty signature is accepted without an Ed25519 call.
///
/// The walker stops at the FIRST failure — `first_bad_height` is the
/// block that tripped the check.
pub fn verify_chain_with_signatures(blocks: &[Block]) -> SignedChainVerifyOutcome {
    let checked = blocks.len() as u64;
    match verify_chain(blocks) {
        VerifyReport::Inconsistent { block_height, .. } => SignedChainVerifyOutcome {
            checked,
            ok: false,
            first_bad_height: Some(block_height),
            signature_failure: false,
        },
        VerifyReport::Ok => {
            for block in blocks {
                let Some(signed) = &block.signed else {
                    // No signed fields present — pure chain block. The
                    // chain verifier already accepted it; nothing more
                    // to check.
                    continue;
                };
                if block.block_height == 0
                    && is_genesis_signed_marker(&signed.signer_pubkey, &signed.signature)
                {
                    continue;
                }
                if signed.signature.len() != SIGNATURE_LEN {
                    return SignedChainVerifyOutcome {
                        checked,
                        ok: false,
                        first_bad_height: Some(block.block_height),
                        signature_failure: true,
                    };
                }
                let mut sig_arr = [0u8; SIGNATURE_LEN];
                sig_arr.copy_from_slice(&signed.signature);
                if reverify_row(&signed.signer_pubkey, &sig_arr, &block.payload).is_err() {
                    return SignedChainVerifyOutcome {
                        checked,
                        ok: false,
                        first_bad_height: Some(block.block_height),
                        signature_failure: true,
                    };
                }
            }
            SignedChainVerifyOutcome::ok(checked)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::signed_writes::{
        verify_insert, InsertSignatureFields, SignedWriteError, SignerRegistry,
    };
    use ed25519_dalek::{Signer, SigningKey};

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pubkey_of(sk: &SigningKey) -> [u8; SIGNER_PUBKEY_LEN] {
        sk.verifying_key().to_bytes()
    }

    /// Build a chain: genesis (null sig) + N signed blocks signed by `sk`.
    fn build_signed_chain<const N: usize>(sk: &SigningKey, payloads: [&[u8]; N]) -> Vec<Block> {
        let mut out: Vec<Block> = Vec::new();
        let mut prev = GENESIS_PREV_HASH;
        // Genesis.
        let g_hash = compute_block_hash(
            &prev,
            0,
            1_000,
            &[],
            Some(&SignedFields {
                signer_pubkey: GENESIS_SIGNER_PUBKEY,
                signature: GENESIS_SIGNATURE.to_vec(),
            }),
        );
        out.push(Block {
            block_height: 0,
            prev_hash: prev,
            timestamp_ms: 1_000,
            payload: Vec::new(),
            signed: Some(SignedFields {
                signer_pubkey: GENESIS_SIGNER_PUBKEY,
                signature: GENESIS_SIGNATURE.to_vec(),
            }),
            hash: g_hash,
        });
        prev = g_hash;
        let pk = pubkey_of(sk);
        for (i, &payload) in payloads.iter().enumerate() {
            let height = (i + 1) as u64;
            let ts = 1_000 + height;
            let sig = sk.sign(payload).to_bytes();
            let signed = SignedFields {
                signer_pubkey: pk,
                signature: sig.to_vec(),
            };
            let hash = compute_block_hash(&prev, height, ts, payload, Some(&signed));
            out.push(Block {
                block_height: height,
                prev_hash: prev,
                timestamp_ms: ts,
                payload: payload.to_vec(),
                signed: Some(signed),
                hash,
            });
            prev = hash;
        }
        out
    }

    #[test]
    fn reserved_columns_signed_chain_is_union() {
        // Locks the contract: a signed-chain row carries six reserved cols.
        assert_eq!(RESERVED_COLUMNS_SIGNED_CHAIN.len(), 6);
        for col in [
            COL_BLOCK_HEIGHT,
            COL_PREV_HASH,
            COL_TIMESTAMP,
            COL_HASH,
            RESERVED_SIGNER_PUBKEY_COL,
            RESERVED_SIGNATURE_COL,
        ] {
            assert!(
                RESERVED_COLUMNS_SIGNED_CHAIN.contains(&col),
                "missing reserved column {col}"
            );
        }
    }

    #[test]
    fn genesis_uses_null_pubkey_and_signature() {
        // Acceptance: "Genesis block uses null pubkey + null signature
        // (documented exemption)".
        let fields = genesis_signed_fields(1_700_000_000_000);
        let pk = fields
            .iter()
            .find(|(k, _)| k == RESERVED_SIGNER_PUBKEY_COL)
            .unwrap();
        match &pk.1 {
            Value::Blob(b) => assert_eq!(&b[..], &GENESIS_SIGNER_PUBKEY[..]),
            other => panic!("signer_pubkey must be Blob, got {other:?}"),
        }
        let sig = fields
            .iter()
            .find(|(k, _)| k == RESERVED_SIGNATURE_COL)
            .unwrap();
        match &sig.1 {
            Value::Blob(b) => {
                assert_eq!(b.len(), SIGNATURE_LEN);
                assert!(b.iter().all(|x| *x == 0));
            }
            other => panic!("signature must be Blob, got {other:?}"),
        }
        let height = fields.iter().find(|(k, _)| k == COL_BLOCK_HEIGHT).unwrap();
        assert_eq!(height.1, Value::UnsignedInteger(0));
    }

    #[test]
    fn hash_binds_signer_pubkey_and_signature() {
        // Acceptance: "hash includes signer_pubkey + signature".
        let sk = signing_key(7);
        let pk = pubkey_of(&sk);
        let payload = b"row=a;";
        let sig = sk.sign(payload).to_bytes().to_vec();
        let (_fields, hash_with_sig) =
            make_signed_block_reserved_fields(GENESIS_PREV_HASH, 1, 42, payload, pk, sig.clone());
        // Flip one byte of the signature → hash changes.
        let mut sig_tampered = sig.clone();
        sig_tampered[0] ^= 0x01;
        let (_f2, hash_tampered) =
            make_signed_block_reserved_fields(GENESIS_PREV_HASH, 1, 42, payload, pk, sig_tampered);
        assert_ne!(hash_with_sig, hash_tampered);
        // Flip one byte of the pubkey → hash changes.
        let mut pk_tampered = pk;
        pk_tampered[0] ^= 0x01;
        let (_f3, hash_pk_tampered) =
            make_signed_block_reserved_fields(GENESIS_PREV_HASH, 1, 42, payload, pk_tampered, sig);
        assert_ne!(hash_with_sig, hash_pk_tampered);
    }

    #[test]
    fn valid_signed_chain_verifies_ok() {
        let sk = signing_key(3);
        let chain = build_signed_chain(&sk, [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()]);
        let out = verify_chain_with_signatures(&chain);
        assert!(out.ok, "{out:?}");
        assert_eq!(out.checked, 4);
        assert!(out.first_bad_height.is_none());
    }

    #[test]
    fn tampering_signer_pubkey_fails_at_block_height() {
        // Acceptance: "Tampering with signer_pubkey → verify_chain fails
        // at that height."
        let sk = signing_key(4);
        let mut chain =
            build_signed_chain(&sk, [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()]);
        // Tamper height-2 signer pubkey. Hash stored is now stale, so
        // verify_chain catches it as a hash mismatch.
        if let Some(signed) = chain[2].signed.as_mut() {
            signed.signer_pubkey[0] ^= 0x55;
        }
        let out = verify_chain_with_signatures(&chain);
        assert!(!out.ok);
        assert_eq!(out.first_bad_height, Some(2));
    }

    #[test]
    fn tampering_signature_with_recomputed_hash_caught_by_sig_reverify() {
        // Even if the attacker re-computes the stored hash so the chain
        // re-links cleanly, signature reverification rejects the forged
        // row.
        let sk = signing_key(5);
        let attacker = signing_key(6);
        let mut chain =
            build_signed_chain(&sk, [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()]);
        // Forge height-2: keep the original pubkey but install a sig
        // produced by the attacker's key over the same payload. The
        // signature is well-formed (64 bytes) but does NOT verify under
        // the legitimate pubkey.
        let target = &mut chain[2];
        let bad_sig = attacker.sign(&target.payload).to_bytes().to_vec();
        target.signed = Some(SignedFields {
            signer_pubkey: pubkey_of(&sk),
            signature: bad_sig,
        });
        // Recompute hash so the chain still links by hash → only
        // signature reverify can catch the forgery.
        let recomputed = compute_block_hash(
            &target.prev_hash,
            target.block_height,
            target.timestamp_ms,
            &target.payload,
            target.signed.as_ref(),
        );
        target.hash = recomputed;
        // Fix downstream blocks' prev_hash + hash so the chain links
        // end-to-end.
        let mut prev = recomputed;
        for i in 3..chain.len() {
            chain[i].prev_hash = prev;
            chain[i].hash = compute_block_hash(
                &chain[i].prev_hash,
                chain[i].block_height,
                chain[i].timestamp_ms,
                &chain[i].payload,
                chain[i].signed.as_ref(),
            );
            prev = chain[i].hash;
        }
        let out = verify_chain_with_signatures(&chain);
        assert!(!out.ok);
        assert_eq!(out.first_bad_height, Some(2));
        assert!(
            out.signature_failure,
            "expected signature_failure, got {out:?}"
        );
    }

    #[test]
    fn composition_chain_fail_then_sig_fail_atomic_reject() {
        // Acceptance: "Valid sig + stale prev_hash → 409 ChainConflict;
        // sig 'not consumed'". And the dual: "Valid chain + bad sig →
        // 401 InvalidSignature; tip unchanged."
        //
        // This module owns the verify side; the INSERT-time composition
        // lives in #522/#524. We pin the contract here as a pure-logic
        // check on the validator order — `verify_insert` is independent
        // of chain state, so a sig failure does not depend on whether
        // the chain check would have passed, and a chain failure does
        // not consume the signature (verify_insert is a pure function).
        let sk = signing_key(8);
        let pk = pubkey_of(&sk);
        let payload = b"payload";
        let sig = sk.sign(payload).to_bytes();
        let registry = SignerRegistry::from_initial(&[pk], "@system", 0);

        // Bad sig → InvalidSignature regardless of chain state.
        let attacker = signing_key(9);
        let bad_sig = attacker.sign(payload).to_bytes();
        let err = verify_insert(
            &registry,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&bad_sig),
            },
            payload,
        )
        .unwrap_err();
        assert_eq!(err, SignedWriteError::InvalidSignature);

        // Valid sig accepted — same registry, same payload.
        verify_insert(
            &registry,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&sig),
            },
            payload,
        )
        .unwrap();
    }

    #[test]
    fn missing_signature_fields_typed_error() {
        // Acceptance: "INSERT requires both chain + signature fields;
        // missing → typed error."
        let registry = SignerRegistry::default();
        let err =
            verify_insert(&registry, &InsertSignatureFields::default(), b"payload").unwrap_err();
        match err {
            SignedWriteError::MissingSignatureFields { fields } => {
                assert!(fields.contains(&RESERVED_SIGNER_PUBKEY_COL));
                assert!(fields.contains(&RESERVED_SIGNATURE_COL));
            }
            other => panic!("expected MissingSignatureFields, got {other:?}"),
        }
    }

    #[test]
    fn genesis_marker_recognised() {
        assert!(is_genesis_signed_marker(
            &GENESIS_SIGNER_PUBKEY,
            &GENESIS_SIGNATURE
        ));
        assert!(!is_genesis_signed_marker(&[1u8; 32], &GENESIS_SIGNATURE));
        let nonzero = [1u8; SIGNATURE_LEN];
        assert!(!is_genesis_signed_marker(&GENESIS_SIGNER_PUBKEY, &nonzero));
    }
}
