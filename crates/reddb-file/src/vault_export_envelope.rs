//! Logical vault export envelope framing (`RDVX`).
//!
//! `red dump` writes the auth vault as a self-contained, hex-encoded
//! AES-256-GCM blob inside the JSONL dump; `red restore` reads it back. This
//! module owns ONLY the envelope framing and its hex transport:
//!
//! ```text
//!   [ 4 bytes: magic "RDVX"                ]
//!   [ 1 byte : version = 1                 ]
//!   [16 bytes: salt (key derivation)       ]
//!   [12 bytes: nonce                       ]
//!   [ N bytes: AES-256-GCM ciphertext+tag  ]
//! ```
//!
//! The whole thing is hex-encoded so it can live inside JSONL dumps.
//!
//! Key derivation, AES-256-GCM encrypt/decrypt, and the `Vault`/`VaultState`
//! types stay in `reddb-server`: this crate never sees the wrapping key or the
//! plaintext. The salt and nonce are embedded so a passphrase-based import can
//! re-derive the same wrapping key without access to the source `.rdb` pages.
//!
//! The AAD string is exported here because it is part of the on-the-wire
//! contract — the server passes it to `aes256_gcm_encrypt`/`decrypt`. Changing
//! the magic, version, field layout, or AAD breaks decryption of every dump
//! sealed by a previous build, so this format is frozen.

/// Logical export blob magic. Distinct from the on-disk vault page chain
/// (`RDVT`/`RDVD`), which is a separate format with its own AAD.
pub const VAULT_LOGICAL_EXPORT_MAGIC: &[u8; 4] = b"RDVX";

/// Current logical export envelope version.
pub const VAULT_LOGICAL_EXPORT_VERSION: u8 = 1;

/// AAD bound into the AES-256-GCM tag of the export blob. Frozen: changing it
/// breaks decryption of existing dumps.
pub const VAULT_LOGICAL_EXPORT_AAD: &[u8] = b"reddb-vault-logical-export-v1";

/// Key-derivation salt size embedded in the envelope.
pub const VAULT_EXPORT_SALT_SIZE: usize = 16;

/// AES-256-GCM nonce size embedded in the envelope.
pub const VAULT_EXPORT_NONCE_SIZE: usize = 12;

const MAGIC_SIZE: usize = 4;
const VERSION_SIZE: usize = 1;

/// AES-256-GCM authentication tag length — the minimum ciphertext we accept.
const GCM_TAG_SIZE: usize = 16;

/// Errors produced while decoding a logical export envelope.
///
/// The server maps each of these onto its own `VaultError::Corrupt(..)` so the
/// operator-facing messages are unchanged after the framing moved here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultExportEnvelopeError {
    /// Outer hex transport did not decode.
    BadHex,
    /// Decoded blob is shorter than the fixed header + minimum ciphertext.
    TooShort,
    /// Magic bytes were not `RDVX`.
    BadMagic,
    /// Version byte is not a supported envelope version.
    UnsupportedVersion(u8),
}

impl std::fmt::Display for VaultExportEnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultExportEnvelopeError::BadHex => write!(f, "bad hex"),
            VaultExportEnvelopeError::TooShort => write!(f, "logical vault export too short"),
            VaultExportEnvelopeError::BadMagic => write!(f, "bad logical vault export magic"),
            VaultExportEnvelopeError::UnsupportedVersion(v) => {
                write!(f, "unsupported logical vault export version: {v}")
            }
        }
    }
}

impl std::error::Error for VaultExportEnvelopeError {}

/// Decoded envelope: the embedded salt, nonce, and the raw ciphertext+tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultExportEnvelope {
    pub salt: [u8; VAULT_EXPORT_SALT_SIZE],
    pub nonce: [u8; VAULT_EXPORT_NONCE_SIZE],
    pub ciphertext: Vec<u8>,
}

/// Frame a sealed export into its hex-encoded transport form.
///
/// `ciphertext` is the AES-256-GCM output (ciphertext followed by the 16-byte
/// tag); this function only lays out the bytes and hex-encodes them.
pub fn encode(
    salt: &[u8; VAULT_EXPORT_SALT_SIZE],
    nonce: &[u8; VAULT_EXPORT_NONCE_SIZE],
    ciphertext: &[u8],
) -> String {
    let mut out = Vec::with_capacity(
        MAGIC_SIZE
            + VERSION_SIZE
            + VAULT_EXPORT_SALT_SIZE
            + VAULT_EXPORT_NONCE_SIZE
            + ciphertext.len(),
    );
    out.extend_from_slice(VAULT_LOGICAL_EXPORT_MAGIC);
    out.push(VAULT_LOGICAL_EXPORT_VERSION);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out.extend_from_slice(ciphertext);
    hex::encode(out)
}

/// Parse a hex-encoded export back into salt, nonce, and ciphertext+tag.
///
/// Validates the hex transport, fixed-length header, magic, and version. The
/// ciphertext is returned untouched; the server is responsible for AES-256-GCM
/// authentication (which is what actually rejects a tampered blob).
pub fn decode(blob_hex: &str) -> Result<VaultExportEnvelope, VaultExportEnvelopeError> {
    let blob = hex::decode(blob_hex).map_err(|_| VaultExportEnvelopeError::BadHex)?;
    let min_len =
        MAGIC_SIZE + VERSION_SIZE + VAULT_EXPORT_SALT_SIZE + VAULT_EXPORT_NONCE_SIZE + GCM_TAG_SIZE;
    if blob.len() < min_len {
        return Err(VaultExportEnvelopeError::TooShort);
    }
    if &blob[0..MAGIC_SIZE] != VAULT_LOGICAL_EXPORT_MAGIC {
        return Err(VaultExportEnvelopeError::BadMagic);
    }
    let version = blob[MAGIC_SIZE];
    if version != VAULT_LOGICAL_EXPORT_VERSION {
        return Err(VaultExportEnvelopeError::UnsupportedVersion(version));
    }

    let mut off = MAGIC_SIZE + VERSION_SIZE;
    let mut salt = [0u8; VAULT_EXPORT_SALT_SIZE];
    salt.copy_from_slice(&blob[off..off + VAULT_EXPORT_SALT_SIZE]);
    off += VAULT_EXPORT_SALT_SIZE;
    let mut nonce = [0u8; VAULT_EXPORT_NONCE_SIZE];
    nonce.copy_from_slice(&blob[off..off + VAULT_EXPORT_NONCE_SIZE]);
    off += VAULT_EXPORT_NONCE_SIZE;
    Ok(VaultExportEnvelope {
        salt,
        nonce,
        ciphertext: blob[off..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field layout is frozen: magic + version + 16B salt + 12B nonce + ct.
    #[test]
    fn encode_lays_out_frozen_header() {
        let salt = [0xABu8; VAULT_EXPORT_SALT_SIZE];
        let nonce = [0xCDu8; VAULT_EXPORT_NONCE_SIZE];
        let ciphertext = vec![0xEFu8; GCM_TAG_SIZE + 8];

        let hexed = encode(&salt, &nonce, &ciphertext);
        let raw = hex::decode(&hexed).unwrap();

        assert_eq!(&raw[0..4], b"RDVX");
        assert_eq!(raw[4], 1);
        assert_eq!(&raw[5..21], &salt);
        assert_eq!(&raw[21..33], &nonce);
        assert_eq!(&raw[33..], &ciphertext[..]);
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let salt = [7u8; VAULT_EXPORT_SALT_SIZE];
        let nonce = [9u8; VAULT_EXPORT_NONCE_SIZE];
        let ciphertext: Vec<u8> = (0..(GCM_TAG_SIZE as u8 + 40)).collect();

        let hexed = encode(&salt, &nonce, &ciphertext);
        let decoded = decode(&hexed).unwrap();

        assert_eq!(decoded.salt, salt);
        assert_eq!(decoded.nonce, nonce);
        assert_eq!(decoded.ciphertext, ciphertext);
        // Re-encoding the decoded parts yields the exact same transport string.
        assert_eq!(
            encode(&decoded.salt, &decoded.nonce, &decoded.ciphertext),
            hexed
        );
    }

    /// A blob frozen by an earlier build must still decode byte-identically.
    /// This hex was produced by the pre-relocation layout; if it ever stops
    /// decoding, the on-disk format silently changed.
    #[test]
    fn decodes_pinned_legacy_blob() {
        // magic 52445658 | ver 01 | salt 16×11 | nonce 12×22 | ct "deadbeef…"
        let salt = [0x11u8; VAULT_EXPORT_SALT_SIZE];
        let nonce = [0x22u8; VAULT_EXPORT_NONCE_SIZE];
        let ciphertext = vec![0xDEu8; GCM_TAG_SIZE + 4];
        let mut raw = Vec::new();
        raw.extend_from_slice(b"RDVX");
        raw.push(1);
        raw.extend_from_slice(&salt);
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&ciphertext);
        let pinned = hex::encode(&raw);

        let decoded = decode(&pinned).unwrap();
        assert_eq!(decoded.salt, salt);
        assert_eq!(decoded.nonce, nonce);
        assert_eq!(decoded.ciphertext, ciphertext);
    }

    #[test]
    fn rejects_bad_hex() {
        assert_eq!(decode("zz not hex"), Err(VaultExportEnvelopeError::BadHex));
    }

    #[test]
    fn rejects_short_blob() {
        let short = hex::encode(b"RDVX\x01tooshort");
        assert_eq!(decode(&short), Err(VaultExportEnvelopeError::TooShort));
    }

    #[test]
    fn rejects_bad_magic() {
        let salt = [0u8; VAULT_EXPORT_SALT_SIZE];
        let nonce = [0u8; VAULT_EXPORT_NONCE_SIZE];
        let ct = vec![0u8; GCM_TAG_SIZE];
        let mut raw = Vec::new();
        raw.extend_from_slice(b"XXXX");
        raw.push(1);
        raw.extend_from_slice(&salt);
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&ct);
        assert_eq!(
            decode(&hex::encode(&raw)),
            Err(VaultExportEnvelopeError::BadMagic)
        );
    }

    #[test]
    fn rejects_unsupported_version() {
        let salt = [0u8; VAULT_EXPORT_SALT_SIZE];
        let nonce = [0u8; VAULT_EXPORT_NONCE_SIZE];
        let ct = vec![0u8; GCM_TAG_SIZE];
        let mut raw = Vec::new();
        raw.extend_from_slice(b"RDVX");
        raw.push(9);
        raw.extend_from_slice(&salt);
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&ct);
        assert_eq!(
            decode(&hex::encode(&raw)),
            Err(VaultExportEnvelopeError::UnsupportedVersion(9))
        );
    }
}
