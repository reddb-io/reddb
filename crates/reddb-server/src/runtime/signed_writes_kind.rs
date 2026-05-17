//! Issue #522 — runtime wiring for `CREATE COLLECTION ... SIGNED_BY (...)`
//! collections.
//!
//! The pure logic — registry + verify_insert + error taxonomy — lives in
//! [`crate::storage::signed_writes`]. This module is the thin adapter
//! that:
//!
//! 1. Persists the per-collection signer registry on the existing
//!    `red_config` config tree under
//!    `red.collection.{name}.signed_writes.*` so it survives restarts.
//! 2. Loads the registry on demand for the INSERT-time verification
//!    path and the `ALTER COLLECTION ... ADD|REVOKE SIGNER` executor.
//! 3. Builds the canonical bytes the client must have signed by
//!    reusing the engine's existing canonical-payload encoding
//!    ([`super::blockchain_kind::canonical_payload`]) with the two
//!    signed-writes reserved columns stripped — same encoding the
//!    blockchain hash binds, so no new on-the-wire spec is introduced.

use crate::storage::schema::Value;
use crate::storage::signed_writes::{
    verify_insert, InsertSignatureFields, SignedWriteError, SignerHistoryAction,
    SignerHistoryEntry, SignerRegistry, RESERVED_SIGNATURE_COL, RESERVED_SIGNER_PUBKEY_COL,
    SIGNATURE_LEN, SIGNER_PUBKEY_LEN,
};
use crate::storage::unified::UnifiedStore;

use std::time::{SystemTime, UNIX_EPOCH};

/// Marker stored at `red.collection.{name}.signed_writes.enabled = true`
/// when the collection was created with a non-empty `SIGNED_BY` list.
const ENABLED_SUFFIX: &str = "signed_writes.enabled";

/// Single JSON-encoded array of currently-allowed Ed25519 pubkeys
/// (lowercase hex). Stored as one text value to keep the read path one
/// `get_config` hop instead of a tree scan.
const ALLOWED_SUFFIX: &str = "signed_writes.allowed_json";

/// Single JSON-encoded array of [`SignerHistoryEntry`] records.
/// Append-only; revoke pushes a `Revoke` entry rather than rewriting the
/// `Add` row, so the audit trail is preserved across registry
/// mutations.
const HISTORY_SUFFIX: &str = "signed_writes.history_json";

fn key(name: &str, suffix: &str) -> String {
    format!("red.collection.{name}.{suffix}")
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode_32(s: &str) -> Option<[u8; SIGNER_PUBKEY_LEN]> {
    if s.len() != SIGNER_PUBKEY_LEN * 2 {
        return None;
    }
    let mut out = [0u8; SIGNER_PUBKEY_LEN];
    for i in 0..SIGNER_PUBKEY_LEN {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn action_str(a: SignerHistoryAction) -> &'static str {
    match a {
        SignerHistoryAction::Add => "add",
        SignerHistoryAction::Revoke => "revoke",
    }
}

fn action_from_str(s: &str) -> Option<SignerHistoryAction> {
    match s {
        "add" => Some(SignerHistoryAction::Add),
        "revoke" => Some(SignerHistoryAction::Revoke),
        _ => None,
    }
}

fn entry_to_json(e: &SignerHistoryEntry) -> crate::serde_json::Value {
    let mut obj = crate::serde_json::Map::new();
    obj.insert(
        "action".to_string(),
        crate::serde_json::Value::String(action_str(e.action).to_string()),
    );
    obj.insert(
        "pubkey".to_string(),
        crate::serde_json::Value::String(hex_encode(&e.pubkey)),
    );
    obj.insert(
        "actor".to_string(),
        crate::serde_json::Value::String(e.actor.clone()),
    );
    obj.insert(
        "ts_unix_ms".to_string(),
        crate::serde_json::Value::Number(e.ts_unix_ms as f64),
    );
    crate::serde_json::Value::Object(obj)
}

fn entry_from_json(v: &crate::serde_json::Value) -> Option<SignerHistoryEntry> {
    let obj = v.as_object()?;
    let action = action_from_str(obj.get("action")?.as_str()?)?;
    let pubkey = hex_decode_32(obj.get("pubkey")?.as_str()?)?;
    let actor = obj.get("actor")?.as_str()?.to_string();
    let ts_unix_ms = obj.get("ts_unix_ms")?.as_u64()? as u128;
    Some(SignerHistoryEntry {
        action,
        pubkey,
        actor,
        ts_unix_ms,
    })
}

/// Returns true if `CREATE COLLECTION ... SIGNED_BY (...)` was issued
/// (or `ALTER COLLECTION ... ADD SIGNER` has been used to enable the
/// registry) and there is at least a marker in `red_config`.
pub fn is_signed(store: &UnifiedStore, collection: &str) -> bool {
    matches!(
        store.get_config(&key(collection, ENABLED_SUFFIX)),
        Some(Value::Boolean(true)) | Some(Value::Text(_))
    )
}

/// Persist the registry-bearing marker plus the initial allowed-signer
/// list. Idempotent: re-calling with the same list is a no-op if a
/// registry is already installed.
pub fn install(
    store: &UnifiedStore,
    collection: &str,
    initial: &[[u8; SIGNER_PUBKEY_LEN]],
    actor: &str,
) {
    if is_signed(store, collection) {
        return;
    }
    let reg = SignerRegistry::from_initial(initial, actor.to_string(), now_ms());
    write_registry(store, collection, &reg);
    // Mark enabled last so a partial install never leaves the marker
    // without payload.
    store.set_config_tree(
        &key(collection, ENABLED_SUFFIX),
        &crate::serde_json::Value::Bool(true),
    );
}

/// Serialise registry state into the config tree. Overwrites any prior
/// value at the same key — the store treats `red_config` as
/// insert-only, but the read path returns the most recent matching row.
fn write_registry(store: &UnifiedStore, collection: &str, reg: &SignerRegistry) {
    let allowed: Vec<crate::serde_json::Value> = reg
        .allowed()
        .map(|pk| crate::serde_json::Value::String(hex_encode(pk)))
        .collect();
    let history: Vec<crate::serde_json::Value> =
        reg.history().iter().map(entry_to_json).collect();
    store.set_config_tree(
        &key(collection, ALLOWED_SUFFIX),
        &crate::serde_json::Value::String(crate::serde_json::Value::Array(allowed).to_string()),
    );
    store.set_config_tree(
        &key(collection, HISTORY_SUFFIX),
        &crate::serde_json::Value::String(crate::serde_json::Value::Array(history).to_string()),
    );
}

/// Read the *latest* value stored under a `red_config` key.
///
/// `UnifiedStore::get_config` returns the *first* matching row, which
/// for append-only configs means the oldest write wins. Registry
/// mutations need the newest write, so we scan and keep the last
/// match.
fn read_latest_config(store: &UnifiedStore, full_key: &str) -> Option<Value> {
    let manager = store.get_collection("red_config")?;
    // `red_config` is append-only: every set rewrites by appending a new
    // row. The growing-segment iterator backs entities with a HashMap so
    // iteration order is non-deterministic — sort by the engine-assigned
    // monotonic `EntityId` descending and take the first match to get
    // the most recent write.
    let mut all = manager.query_all(|_| true);
    all.sort_by(|a, b| b.id.raw().cmp(&a.id.raw()));
    for entity in all {
        let crate::storage::unified::EntityData::Row(row) = &entity.data else {
            continue;
        };
        let Some(named) = &row.named else { continue };
        let matches = matches!(
            named.get("key"),
            Some(Value::Text(s)) if s.as_ref() == full_key
        );
        if matches {
            return named.get("value").cloned();
        }
    }
    None
}

fn read_registry(store: &UnifiedStore, collection: &str) -> SignerRegistry {
    let allowed_json = match read_latest_config(store, &key(collection, ALLOWED_SUFFIX)) {
        Some(Value::Text(s)) => s.to_string(),
        _ => "[]".to_string(),
    };
    let history_json = match read_latest_config(store, &key(collection, HISTORY_SUFFIX)) {
        Some(Value::Text(s)) => s.to_string(),
        _ => "[]".to_string(),
    };
    let parsed_allowed: Vec<[u8; SIGNER_PUBKEY_LEN]> = match crate::utils::json::parse_json(
        &allowed_json,
    ) {
        Ok(v) => match crate::serde_json::Value::from(v) {
            crate::serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().and_then(hex_decode_32))
                .collect(),
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    let parsed_history: Vec<SignerHistoryEntry> = match crate::utils::json::parse_json(
        &history_json,
    ) {
        Ok(v) => match crate::serde_json::Value::from(v) {
            crate::serde_json::Value::Array(arr) => {
                arr.iter().filter_map(entry_from_json).collect()
            }
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    SignerRegistry::from_persisted_parts(parsed_allowed, parsed_history)
}

/// Load the current registry. Cheap enough for the INSERT hot path:
/// two `red_config` reads + two JSON parses, no scan of the parent
/// collection.
pub fn registry(store: &UnifiedStore, collection: &str) -> SignerRegistry {
    read_registry(store, collection)
}

/// Apply `ALTER COLLECTION ... ADD SIGNER 'hex'` on a signed-writes
/// collection. Returns `true` if the registry actually changed.
pub fn add_signer(
    store: &UnifiedStore,
    collection: &str,
    pubkey: [u8; SIGNER_PUBKEY_LEN],
    actor: &str,
) -> bool {
    let mut reg = read_registry(store, collection);
    let changed = reg.add_signer(pubkey, actor.to_string(), now_ms());
    if changed {
        write_registry(store, collection, &reg);
    }
    changed
}

/// Apply `ALTER COLLECTION ... REVOKE SIGNER 'hex'` on a signed-writes
/// collection. Returns `true` if the key was previously allowed.
pub fn revoke_signer(
    store: &UnifiedStore,
    collection: &str,
    pubkey: &[u8; SIGNER_PUBKEY_LEN],
    actor: &str,
) -> bool {
    let mut reg = read_registry(store, collection);
    let changed = reg.revoke_signer(pubkey, actor.to_string(), now_ms());
    if changed {
        write_registry(store, collection, &reg);
    }
    changed
}

/// Reserved column set automatically present on every signed-writes
/// collection. Filtered out of the canonical-payload bytes the client
/// signs.
pub const RESERVED_COLUMNS: &[&str] = &[RESERVED_SIGNER_PUBKEY_COL, RESERVED_SIGNATURE_COL];

/// Pulled-apart signer / signature reserved columns. Carries:
///
/// * The user's original `Value` (for round-trip storage so SELECT and
///   `WHERE signer_pubkey = '<hex>'` predicates compare against the
///   same encoding the caller supplied — Text-typed hex on the JSON /
///   SQL path, Blob on the binary protobuf path).
/// * The decoded raw bytes used to verify the Ed25519 signature.
pub struct SignerColumn {
    pub raw_value: Value,
    pub bytes: Vec<u8>,
}

/// Pull the `signer_pubkey` and `signature` values out of the row's
/// fields. Returns the parsed reserved columns + the residual field
/// list (fields stripped of the two reserved columns) — the residual
/// goes into the canonical payload.
pub fn split_signature_fields(
    fields: Vec<(String, Value)>,
) -> (Option<SignerColumn>, Option<SignerColumn>, Vec<(String, Value)>) {
    let mut pubkey: Option<SignerColumn> = None;
    let mut signature: Option<SignerColumn> = None;
    let mut residual: Vec<(String, Value)> = Vec::with_capacity(fields.len());
    for (k, v) in fields {
        if k == RESERVED_SIGNER_PUBKEY_COL {
            let bytes = match &v {
                Value::Blob(b) => Some(b.clone()),
                // Accept hex-encoded pubkey from JSON / SQL callers
                // that can't easily express literal blobs.
                Value::Text(s) => decode_hex(s.as_ref()),
                _ => None,
            };
            if let Some(bytes) = bytes {
                pubkey = Some(SignerColumn { raw_value: v, bytes });
            }
            continue;
        }
        if k == RESERVED_SIGNATURE_COL {
            let bytes = match &v {
                Value::Blob(b) => Some(b.clone()),
                Value::Text(s) => decode_hex(s.as_ref()),
                _ => None,
            };
            if let Some(bytes) = bytes {
                signature = Some(SignerColumn { raw_value: v, bytes });
            }
            continue;
        }
        residual.push((k, v));
    }
    (pubkey, signature, residual)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        out.push(u8::from_str_radix(&s[i..i + 2], 16).ok()?);
    }
    Some(out)
}

/// Top-level INSERT-time check used by the engine. Computes the
/// canonical payload from the (already reserved-column-stripped)
/// residual fields and dispatches to [`verify_insert`].
pub fn verify_row(
    registry: &SignerRegistry,
    signer_pubkey: Option<&[u8]>,
    signature: Option<&[u8]>,
    canonical_payload: &[u8],
) -> Result<(), SignedWriteError> {
    verify_insert(
        registry,
        &InsertSignatureFields {
            signer_pubkey,
            signature,
        },
        canonical_payload,
    )
}

/// Map a [`SignedWriteError`] onto a [`RedDBError`] whose marker prefix
/// is picked up by the transport-layer status mapper.
///
/// | Variant                  | Prefix                                     | HTTP |
/// |--------------------------|--------------------------------------------|------|
/// | `MissingSignatureFields` | `SignedWriteError:MissingSignatureFields:` | 400  |
/// | `MalformedSignerPubkey`  | `SignedWriteError:MalformedSignerPubkey`   | 400  |
/// | `MalformedSignature`     | `SignedWriteError:MalformedSignature`      | 400  |
/// | `UnknownSigner`          | `SignedWriteError:UnknownSigner`           | 401  |
/// | `RevokedSigner`          | `SignedWriteError:RevokedSigner`           | 401  |
/// | `InvalidSignature`       | `SignedWriteError:InvalidSignature`        | 401  |
pub fn map_error(err: SignedWriteError) -> crate::api::RedDBError {
    let body = match &err {
        SignedWriteError::MissingSignatureFields { fields } => {
            format!("SignedWriteError:MissingSignatureFields:{}", fields.join(","))
        }
        SignedWriteError::UnknownSigner { pubkey } => {
            format!("SignedWriteError:UnknownSigner:{}", hex_encode(pubkey))
        }
        SignedWriteError::RevokedSigner { pubkey } => {
            format!("SignedWriteError:RevokedSigner:{}", hex_encode(pubkey))
        }
        SignedWriteError::InvalidSignature => "SignedWriteError:InvalidSignature".to_string(),
        SignedWriteError::MalformedSignerPubkey => {
            "SignedWriteError:MalformedSignerPubkey".to_string()
        }
        SignedWriteError::MalformedSignature => "SignedWriteError:MalformedSignature".to_string(),
    };
    crate::api::RedDBError::InvalidOperation(body)
}

/// Length sanity: a signature blob must be exactly 64 bytes. Surfaced
/// to the caller so it can return `MalformedSignature` before computing
/// the canonical payload.
pub const SIGNATURE_BYTES: usize = SIGNATURE_LEN;

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pubkey_of(sk: &SigningKey) -> [u8; SIGNER_PUBKEY_LEN] {
        sk.verifying_key().to_bytes()
    }

    fn make_store() -> UnifiedStore {
        UnifiedStore::new()
    }

    #[test]
    fn install_and_read_roundtrip_preserves_registry() {
        let store = make_store();
        let pk1 = pubkey_of(&signing_key(1));
        let pk2 = pubkey_of(&signing_key(2));
        install(&store, "sc", &[pk1, pk2], "@system/create");
        assert!(is_signed(&store, "sc"));
        let reg = registry(&store, "sc");
        assert_eq!(reg.allowed_len(), 2);
        assert!(reg.is_allowed(&pk1));
        assert!(reg.is_allowed(&pk2));
        assert_eq!(reg.history().len(), 2);
    }

    #[test]
    fn add_signer_persists_and_records_history() {
        let store = make_store();
        let pk1 = pubkey_of(&signing_key(1));
        install(&store, "sc", &[pk1], "@system/create");
        let pk2 = pubkey_of(&signing_key(2));
        assert!(add_signer(&store, "sc", pk2, "admin:alice"));
        // Idempotent re-add returns false.
        assert!(!add_signer(&store, "sc", pk2, "admin:alice"));
        let reg = registry(&store, "sc");
        assert!(reg.is_allowed(&pk2));
        assert_eq!(reg.history().len(), 2);
        let last = reg.history().last().unwrap();
        assert_eq!(last.action, SignerHistoryAction::Add);
        assert_eq!(last.actor, "admin:alice");
    }

    #[test]
    fn revoke_signer_blocks_future_inserts_but_history_preserved() {
        let store = make_store();
        let sk = signing_key(7);
        let pk = pubkey_of(&sk);
        install(&store, "sc", &[pk], "@system/create");
        assert!(revoke_signer(&store, "sc", &pk, "admin:bob"));
        let reg = registry(&store, "sc");
        assert!(!reg.is_allowed(&pk));
        assert!(reg.ever_added(&pk));
        let last = reg.history().last().unwrap();
        assert_eq!(last.action, SignerHistoryAction::Revoke);
        assert_eq!(last.actor, "admin:bob");
    }

    #[test]
    fn split_signature_fields_extracts_blob_columns() {
        let fields = vec![
            ("name".to_string(), Value::text("alice".to_string())),
            (RESERVED_SIGNER_PUBKEY_COL.to_string(), Value::Blob(vec![0x11; 32])),
            (RESERVED_SIGNATURE_COL.to_string(), Value::Blob(vec![0x22; 64])),
        ];
        let (pk, sig, residual) = split_signature_fields(fields);
        assert_eq!(pk.as_ref().unwrap().bytes.len(), 32);
        assert!(matches!(pk.unwrap().raw_value, Value::Blob(_)));
        assert_eq!(sig.as_ref().unwrap().bytes.len(), 64);
        assert!(matches!(sig.unwrap().raw_value, Value::Blob(_)));
        assert_eq!(residual.len(), 1);
        assert_eq!(residual[0].0, "name");
    }

    #[test]
    fn split_signature_fields_accepts_hex_text() {
        let pk_hex = "11".repeat(32);
        let sig_hex = "22".repeat(64);
        let fields = vec![
            (RESERVED_SIGNER_PUBKEY_COL.to_string(), Value::text(pk_hex)),
            (RESERVED_SIGNATURE_COL.to_string(), Value::text(sig_hex)),
        ];
        let (pk, sig, residual) = split_signature_fields(fields);
        assert_eq!(pk.as_ref().unwrap().bytes, vec![0x11; 32]);
        assert!(matches!(pk.unwrap().raw_value, Value::Text(_)));
        assert_eq!(sig.as_ref().unwrap().bytes, vec![0x22; 64]);
        assert!(matches!(sig.unwrap().raw_value, Value::Text(_)));
        assert!(residual.is_empty());
    }

    #[test]
    fn map_error_carries_variant_prefix() {
        let pk = [0u8; SIGNER_PUBKEY_LEN];
        match map_error(SignedWriteError::UnknownSigner { pubkey: pk }) {
            crate::api::RedDBError::InvalidOperation(s) => {
                assert!(s.starts_with("SignedWriteError:UnknownSigner"));
            }
            other => panic!("unexpected mapping: {other:?}"),
        }
        match map_error(SignedWriteError::InvalidSignature) {
            crate::api::RedDBError::InvalidOperation(s) => {
                assert_eq!(s, "SignedWriteError:InvalidSignature");
            }
            other => panic!("unexpected mapping: {other:?}"),
        }
    }

    #[test]
    fn verify_row_accepts_valid_signature_over_canonical_payload() {
        let sk = signing_key(3);
        let pk = pubkey_of(&sk);
        let store = make_store();
        install(&store, "sc", &[pk], "@system/create");
        let payload = b"hello-world";
        let sig = sk.sign(payload).to_bytes();
        let reg = registry(&store, "sc");
        verify_row(&reg, Some(&pk), Some(&sig), payload).unwrap();
    }

    #[test]
    fn verify_row_rejects_tampered_payload() {
        let sk = signing_key(4);
        let pk = pubkey_of(&sk);
        let store = make_store();
        install(&store, "sc", &[pk], "@system/create");
        let payload = b"hello-world";
        let sig = sk.sign(payload).to_bytes();
        let reg = registry(&store, "sc");
        let err = verify_row(&reg, Some(&pk), Some(&sig), b"tampered").unwrap_err();
        assert_eq!(err, SignedWriteError::InvalidSignature);
    }
}
