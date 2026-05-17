//! Signed Writes — pure logic for `CREATE COLLECTION ... SIGNED_BY (...)`.
//!
//! A collection with a non-empty signer registry rejects every INSERT
//! that does not carry a valid Ed25519 signature produced by one of the
//! currently-allowed signer keys. This module provides the
//! deterministic, side-effect-free pieces of that contract:
//!
//! * The reserved column names + byte widths injected on `SIGNED_BY`
//!   collections.
//! * A [`SignerRegistry`] holding the *currently* allowed keys plus an
//!   append-only [`SignerHistoryEntry`] log of admin mutations.
//! * The [`SignedWriteError`] taxonomy that the engine maps onto HTTP
//!   400 / 401 responses.
//! * [`verify_insert`] — the single entry point the insert path will
//!   call to validate one row.
//!
//! Issue #522 wires this into the runtime insert path, REST error
//! mapping, and the catalog persistence of the registry. This file is
//! intentionally self-contained so the wiring is a thin adapter on top
//! of audited logic.

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Reserved column auto-added to every signed-writes collection. Holds
/// the 32-byte Ed25519 public key the signer used to sign the row.
pub const RESERVED_SIGNER_PUBKEY_COL: &str = "signer_pubkey";

/// Reserved column auto-added to every signed-writes collection. Holds
/// the 64-byte raw Ed25519 signature over the canonical payload.
pub const RESERVED_SIGNATURE_COL: &str = "signature";

/// Length of a raw Ed25519 public key, in bytes.
pub const SIGNER_PUBKEY_LEN: usize = 32;

/// Length of a raw Ed25519 signature, in bytes.
pub const SIGNATURE_LEN: usize = 64;

/// Failure modes for a signed-writes INSERT. The runtime maps each
/// variant onto an HTTP status:
///
/// | Variant                  | HTTP |
/// |--------------------------|------|
/// | `MissingSignatureFields` |  400 |
/// | `UnknownSigner`          |  401 |
/// | `RevokedSigner`          |  401 |
/// | `InvalidSignature`       |  401 |
/// | `MalformedSignerPubkey`  |  400 |
/// | `MalformedSignature`     |  400 |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignedWriteError {
    /// Row omitted `signer_pubkey` and/or `signature` on a collection
    /// that requires them. Carries the missing column name(s) for the
    /// error response.
    MissingSignatureFields { fields: Vec<&'static str> },
    /// `signer_pubkey` was a valid Ed25519 key but is not in the
    /// collection's current allowed-signer set AND has never appeared
    /// in the history — i.e. an entirely unknown key.
    UnknownSigner { pubkey: [u8; SIGNER_PUBKEY_LEN] },
    /// `signer_pubkey` was previously allowed (appears in history with
    /// an `Add` event) but has since been revoked. Distinguished from
    /// `UnknownSigner` so operators can tell "never seen" from
    /// "revoked" in audit logs.
    RevokedSigner { pubkey: [u8; SIGNER_PUBKEY_LEN] },
    /// Signature parsed as 64 bytes but did NOT verify against the
    /// supplied `signer_pubkey` + canonical payload.
    InvalidSignature,
    /// `signer_pubkey` was present but not 32 bytes / not a valid
    /// Ed25519 public key encoding.
    MalformedSignerPubkey,
    /// `signature` was present but not 64 bytes.
    MalformedSignature,
}

impl std::fmt::Display for SignedWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSignatureFields { fields } => {
                write!(f, "MissingSignatureFields: {}", fields.join(", "))
            }
            Self::UnknownSigner { .. } => f.write_str("UnknownSigner"),
            Self::RevokedSigner { .. } => f.write_str("RevokedSigner"),
            Self::InvalidSignature => f.write_str("InvalidSignature"),
            Self::MalformedSignerPubkey => f.write_str("MalformedSignerPubkey"),
            Self::MalformedSignature => f.write_str("MalformedSignature"),
        }
    }
}

impl std::error::Error for SignedWriteError {}

/// Action recorded in [`SignerRegistry::history`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerHistoryAction {
    /// Initial registration at `CREATE COLLECTION` time or via
    /// `ALTER COLLECTION ... ADD SIGNER`.
    Add,
    /// Removed via `ALTER COLLECTION ... REVOKE SIGNER`. Past rows
    /// signed by this key remain readable and re-verifiable; only
    /// *new* inserts are rejected.
    Revoke,
}

/// One entry in the append-only admin history of a signer registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerHistoryEntry {
    pub action: SignerHistoryAction,
    pub pubkey: [u8; SIGNER_PUBKEY_LEN],
    /// Principal that performed the mutation. Free-form so the
    /// caller can pass user IDs, role names, or system markers like
    /// `"@system/create-collection"` for the genesis entry.
    pub actor: String,
    /// Wall-clock ms-since-epoch the engine recorded when applying
    /// the mutation. The registry never inspects this; it exists for
    /// audit.
    pub ts_unix_ms: u128,
}

/// Mutable signer registry attached to a `SIGNED_BY` collection.
///
/// Invariants:
///
/// 1. `allowed` is the *exact* set of keys that may produce new
///    signatures. Empty set ⇒ collection rejects every insert (the
///    runtime treats an empty `SIGNED_BY` list as a parse error, so
///    in practice this only happens after every key is revoked —
///    intentional kill-switch behaviour).
/// 2. `history` is append-only. `add_signer` / `revoke_signer` push
///    new entries; nothing ever pops.
/// 3. `add_signer` of an already-allowed key is a no-op (no history
///    entry written) so that idempotent DDL replays don't flood the
///    log. `revoke_signer` of an unknown key returns `false`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignerRegistry {
    allowed: BTreeSet<[u8; SIGNER_PUBKEY_LEN]>,
    history: Vec<SignerHistoryEntry>,
}

impl SignerRegistry {
    /// Build a registry from the initial `SIGNED_BY (...)` list parsed
    /// at `CREATE COLLECTION` time. Each key receives one
    /// `SignerHistoryAction::Add` entry with the supplied actor /
    /// timestamp so the audit trail is non-empty from genesis.
    pub fn from_initial(
        initial: &[[u8; SIGNER_PUBKEY_LEN]],
        actor: impl Into<String>,
        ts_unix_ms: u128,
    ) -> Self {
        let actor = actor.into();
        let mut reg = Self::default();
        for pk in initial {
            if reg.allowed.insert(*pk) {
                reg.history.push(SignerHistoryEntry {
                    action: SignerHistoryAction::Add,
                    pubkey: *pk,
                    actor: actor.clone(),
                    ts_unix_ms,
                });
            }
        }
        reg
    }

    /// Rebuild a registry from previously-persisted state. Used by the
    /// runtime adapter when loading the registry off `red_config` — the
    /// caller is responsible for the storage format; this constructor
    /// only stitches the in-memory invariants back together.
    pub fn from_persisted_parts(
        allowed: Vec<[u8; SIGNER_PUBKEY_LEN]>,
        history: Vec<SignerHistoryEntry>,
    ) -> Self {
        Self {
            allowed: allowed.into_iter().collect(),
            history,
        }
    }

    /// Snapshot of the currently-allowed signers, in stable order.
    pub fn allowed(&self) -> impl Iterator<Item = &[u8; SIGNER_PUBKEY_LEN]> {
        self.allowed.iter()
    }

    pub fn allowed_len(&self) -> usize {
        self.allowed.len()
    }

    pub fn history(&self) -> &[SignerHistoryEntry] {
        &self.history
    }

    pub fn is_allowed(&self, pubkey: &[u8; SIGNER_PUBKEY_LEN]) -> bool {
        self.allowed.contains(pubkey)
    }

    /// Returns true if this key was added at any point in the past
    /// (even if later revoked). Used by [`verify_insert`] to
    /// distinguish `UnknownSigner` from `RevokedSigner`.
    pub fn ever_added(&self, pubkey: &[u8; SIGNER_PUBKEY_LEN]) -> bool {
        self.history
            .iter()
            .any(|e| e.action == SignerHistoryAction::Add && &e.pubkey == pubkey)
    }

    /// Add `pubkey` to the allowed set. Returns `true` if the key was
    /// newly added (history entry written), `false` if it was already
    /// allowed (idempotent no-op).
    pub fn add_signer(
        &mut self,
        pubkey: [u8; SIGNER_PUBKEY_LEN],
        actor: impl Into<String>,
        ts_unix_ms: u128,
    ) -> bool {
        if !self.allowed.insert(pubkey) {
            return false;
        }
        self.history.push(SignerHistoryEntry {
            action: SignerHistoryAction::Add,
            pubkey,
            actor: actor.into(),
            ts_unix_ms,
        });
        true
    }

    /// Remove `pubkey` from the allowed set. Returns `true` if the key
    /// was present (and a `Revoke` history entry written), `false` if
    /// it was unknown. Past rows signed by `pubkey` remain valid and
    /// re-verifiable — only future inserts are rejected.
    pub fn revoke_signer(
        &mut self,
        pubkey: &[u8; SIGNER_PUBKEY_LEN],
        actor: impl Into<String>,
        ts_unix_ms: u128,
    ) -> bool {
        if !self.allowed.remove(pubkey) {
            return false;
        }
        self.history.push(SignerHistoryEntry {
            action: SignerHistoryAction::Revoke,
            pubkey: *pubkey,
            actor: actor.into(),
            ts_unix_ms,
        });
        true
    }
}

/// Result of looking at the row-supplied signer + signature columns
/// before verification. `None` for either side means the caller passed
/// `NULL` / omitted column entirely.
#[derive(Debug, Clone, Default)]
pub struct InsertSignatureFields<'a> {
    pub signer_pubkey: Option<&'a [u8]>,
    pub signature: Option<&'a [u8]>,
}

/// Top-level insert-time verification.
///
/// 1. Both columns must be present (else `MissingSignatureFields`).
/// 2. `signer_pubkey` must be exactly 32 bytes and a valid Ed25519
///    point encoding (else `MalformedSignerPubkey`).
/// 3. `signature` must be exactly 64 bytes (else `MalformedSignature`).
/// 4. `signer_pubkey` must be in the registry's *current* allowed set
///    (else `UnknownSigner` or `RevokedSigner` depending on history).
/// 5. The signature must verify against `signer_pubkey` over
///    `canonical_payload` (else `InvalidSignature`).
///
/// `canonical_payload` is the engine's content-hash encoding of the
/// row WITHOUT the reserved `signer_pubkey` / `signature` columns —
/// the same bytes the client signed.
pub fn verify_insert(
    registry: &SignerRegistry,
    fields: &InsertSignatureFields<'_>,
    canonical_payload: &[u8],
) -> Result<(), SignedWriteError> {
    let mut missing: Vec<&'static str> = Vec::new();
    if fields.signer_pubkey.is_none() {
        missing.push(RESERVED_SIGNER_PUBKEY_COL);
    }
    if fields.signature.is_none() {
        missing.push(RESERVED_SIGNATURE_COL);
    }
    if !missing.is_empty() {
        return Err(SignedWriteError::MissingSignatureFields { fields: missing });
    }
    // unwraps safe by the missing-check above.
    let pubkey_bytes = fields.signer_pubkey.unwrap();
    let sig_bytes = fields.signature.unwrap();

    let pubkey_arr: [u8; SIGNER_PUBKEY_LEN] = pubkey_bytes
        .try_into()
        .map_err(|_| SignedWriteError::MalformedSignerPubkey)?;
    if sig_bytes.len() != SIGNATURE_LEN {
        return Err(SignedWriteError::MalformedSignature);
    }
    let sig_arr: [u8; SIGNATURE_LEN] = sig_bytes
        .try_into()
        .map_err(|_| SignedWriteError::MalformedSignature)?;

    if !registry.is_allowed(&pubkey_arr) {
        return Err(if registry.ever_added(&pubkey_arr) {
            SignedWriteError::RevokedSigner { pubkey: pubkey_arr }
        } else {
            SignedWriteError::UnknownSigner { pubkey: pubkey_arr }
        });
    }

    // Construct the verifying key — if pubkey_arr is not a valid
    // Ed25519 point encoding, surface MalformedSignerPubkey.
    let vk = VerifyingKey::from_bytes(&pubkey_arr)
        .map_err(|_| SignedWriteError::MalformedSignerPubkey)?;
    let signature = Signature::from_bytes(&sig_arr);

    vk.verify(canonical_payload, &signature)
        .map_err(|_| SignedWriteError::InvalidSignature)
}

/// Re-verify a previously-accepted row by its stored
/// `signer_pubkey` + `signature` + canonical payload. Used by
/// integrity scans (`/admin/verify-collection`) — does NOT consult the
/// registry, so rows signed by since-revoked keys still re-verify Ok.
/// This is the property the issue calls out:
/// > Insert with revoked signer → 401 RevokedSigner; past records still
/// > readable + re-verifiable
pub fn reverify_row(
    signer_pubkey: &[u8; SIGNER_PUBKEY_LEN],
    signature: &[u8; SIGNATURE_LEN],
    canonical_payload: &[u8],
) -> Result<(), SignedWriteError> {
    let vk = VerifyingKey::from_bytes(signer_pubkey)
        .map_err(|_| SignedWriteError::MalformedSignerPubkey)?;
    let sig = Signature::from_bytes(signature);
    vk.verify(canonical_payload, &sig)
        .map_err(|_| SignedWriteError::InvalidSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn fixed_signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pubkey_bytes(sk: &SigningKey) -> [u8; SIGNER_PUBKEY_LEN] {
        sk.verifying_key().to_bytes()
    }

    #[test]
    fn from_initial_seeds_history_and_allowed_set() {
        let sk_a = fixed_signing_key(1);
        let sk_b = fixed_signing_key(2);
        let reg = SignerRegistry::from_initial(
            &[pubkey_bytes(&sk_a), pubkey_bytes(&sk_b)],
            "@system/create-collection",
            10,
        );
        assert_eq!(reg.allowed_len(), 2);
        assert_eq!(reg.history().len(), 2);
        assert!(reg
            .history()
            .iter()
            .all(|h| h.action == SignerHistoryAction::Add && h.actor == "@system/create-collection"
                && h.ts_unix_ms == 10));
    }

    #[test]
    fn add_signer_is_idempotent() {
        let sk = fixed_signing_key(7);
        let pk = pubkey_bytes(&sk);
        let mut reg = SignerRegistry::default();
        assert!(reg.add_signer(pk, "alice", 1));
        assert!(!reg.add_signer(pk, "alice-again", 2)); // dup → no-op
        assert_eq!(reg.history().len(), 1);
    }

    #[test]
    fn revoke_signer_records_history_and_blocks_future_inserts() {
        let sk = fixed_signing_key(3);
        let pk = pubkey_bytes(&sk);
        let mut reg = SignerRegistry::from_initial(&[pk], "@system", 0);
        assert!(reg.is_allowed(&pk));
        assert!(reg.revoke_signer(&pk, "bob-admin", 100));
        assert!(!reg.is_allowed(&pk));
        assert!(reg.ever_added(&pk));
        assert_eq!(reg.history().len(), 2);
        assert_eq!(reg.history()[1].action, SignerHistoryAction::Revoke);
        // Idempotent revoke of an already-revoked key returns false.
        assert!(!reg.revoke_signer(&pk, "bob-admin", 200));
    }

    #[test]
    fn missing_fields_lists_both_missing() {
        let reg = SignerRegistry::default();
        let err = verify_insert(&reg, &InsertSignatureFields::default(), b"payload").unwrap_err();
        match err {
            SignedWriteError::MissingSignatureFields { fields } => {
                assert!(fields.contains(&RESERVED_SIGNER_PUBKEY_COL));
                assert!(fields.contains(&RESERVED_SIGNATURE_COL));
            }
            other => panic!("expected MissingSignatureFields, got {other:?}"),
        }
    }

    #[test]
    fn missing_signature_only_is_reported() {
        let sk = fixed_signing_key(5);
        let pk = pubkey_bytes(&sk);
        let reg = SignerRegistry::from_initial(&[pk], "@system", 0);
        let err = verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: None,
            },
            b"x",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SignedWriteError::MissingSignatureFields { ref fields }
                if fields == &vec![RESERVED_SIGNATURE_COL]
        ));
    }

    #[test]
    fn unknown_signer_rejected() {
        let sk_allowed = fixed_signing_key(1);
        let sk_stranger = fixed_signing_key(2);
        let reg = SignerRegistry::from_initial(&[pubkey_bytes(&sk_allowed)], "@system", 0);
        let payload = b"hello";
        let sig = sk_stranger.sign(payload).to_bytes();
        let pk = pubkey_bytes(&sk_stranger);
        let err = verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&sig),
            },
            payload,
        )
        .unwrap_err();
        assert_eq!(err, SignedWriteError::UnknownSigner { pubkey: pk });
    }

    #[test]
    fn revoked_signer_distinguished_from_unknown() {
        let sk = fixed_signing_key(9);
        let pk = pubkey_bytes(&sk);
        let mut reg = SignerRegistry::from_initial(&[pk], "@system", 0);
        assert!(reg.revoke_signer(&pk, "ops", 1));
        let payload = b"after-revoke";
        let sig = sk.sign(payload).to_bytes();
        let err = verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&sig),
            },
            payload,
        )
        .unwrap_err();
        assert_eq!(err, SignedWriteError::RevokedSigner { pubkey: pk });
    }

    #[test]
    fn valid_signature_accepted() {
        let sk = fixed_signing_key(4);
        let pk = pubkey_bytes(&sk);
        let reg = SignerRegistry::from_initial(&[pk], "@system", 0);
        let payload = b"row-canon-bytes";
        let sig = sk.sign(payload).to_bytes();
        verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&sig),
            },
            payload,
        )
        .unwrap();
    }

    #[test]
    fn tampered_payload_rejected_as_invalid_signature() {
        let sk = fixed_signing_key(6);
        let pk = pubkey_bytes(&sk);
        let reg = SignerRegistry::from_initial(&[pk], "@system", 0);
        let signed_payload = b"original";
        let sig = sk.sign(signed_payload).to_bytes();
        let err = verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&sig),
            },
            b"tampered",
        )
        .unwrap_err();
        assert_eq!(err, SignedWriteError::InvalidSignature);
    }

    #[test]
    fn malformed_signature_length() {
        let sk = fixed_signing_key(8);
        let pk = pubkey_bytes(&sk);
        let reg = SignerRegistry::from_initial(&[pk], "@system", 0);
        let err = verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&[0u8; 10][..]),
            },
            b"x",
        )
        .unwrap_err();
        assert_eq!(err, SignedWriteError::MalformedSignature);
    }

    #[test]
    fn malformed_signer_pubkey_length() {
        let reg = SignerRegistry::default();
        let err = verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&[0u8; 7][..]),
                signature: Some(&[0u8; SIGNATURE_LEN][..]),
            },
            b"x",
        )
        .unwrap_err();
        assert_eq!(err, SignedWriteError::MalformedSignerPubkey);
    }

    #[test]
    fn past_record_re_verifies_after_signer_revoked() {
        // Acceptance: "past records still readable + re-verifiable"
        // after revoke. `reverify_row` doesn't consult the registry.
        let sk = fixed_signing_key(11);
        let pk = pubkey_bytes(&sk);
        let payload = b"committed-row";
        let sig = sk.sign(payload).to_bytes();

        let mut reg = SignerRegistry::from_initial(&[pk], "@system", 0);
        // Insert succeeded at write time.
        verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&sig),
            },
            payload,
        )
        .unwrap();
        // Operator revokes the signer later.
        reg.revoke_signer(&pk, "ops", 999);
        // Future inserts blocked …
        let blocked = verify_insert(
            &reg,
            &InsertSignatureFields {
                signer_pubkey: Some(&pk),
                signature: Some(&sig),
            },
            payload,
        )
        .unwrap_err();
        assert_eq!(blocked, SignedWriteError::RevokedSigner { pubkey: pk });
        // … but the historical row still re-verifies fine.
        reverify_row(&pk, &sig, payload).unwrap();
    }

    #[test]
    fn error_display_strings_are_stable() {
        // The runtime maps these onto HTTP error bodies; pin the
        // strings so renaming a variant trips a test.
        assert_eq!(
            SignedWriteError::UnknownSigner { pubkey: [0u8; 32] }.to_string(),
            "UnknownSigner"
        );
        assert_eq!(
            SignedWriteError::RevokedSigner { pubkey: [0u8; 32] }.to_string(),
            "RevokedSigner"
        );
        assert_eq!(
            SignedWriteError::InvalidSignature.to_string(),
            "InvalidSignature"
        );
        assert_eq!(
            SignedWriteError::MalformedSignature.to_string(),
            "MalformedSignature"
        );
        assert_eq!(
            SignedWriteError::MalformedSignerPubkey.to_string(),
            "MalformedSignerPubkey"
        );
        assert_eq!(
            SignedWriteError::MissingSignatureFields {
                fields: vec![RESERVED_SIGNER_PUBKEY_COL, RESERVED_SIGNATURE_COL],
            }
            .to_string(),
            format!(
                "MissingSignatureFields: {}, {}",
                RESERVED_SIGNER_PUBKEY_COL, RESERVED_SIGNATURE_COL
            ),
        );
    }
}
