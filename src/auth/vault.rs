//! Encrypted vault for auth state persistence.
//!
//! Stores users and API keys in **reserved pages** inside the main `.rdb`
//! database file instead of a separate `_vault.rdb` file.  The vault
//! occupies pages 2-3 (header + data) using `PageType::Vault` and is
//! encrypted with a SEPARATE key derived from `REDDB_VAULT_KEY`.
//!
//! Page content layout (inside the 4KB page, after the 32-byte page header):
//!   [4 bytes: magic "RDVT"]
//!   [1 byte: version]
//!   [16 bytes: salt (for key derivation)]
//!   [4 bytes: payload length (little-endian u32)]
//!   [12 bytes: nonce]
//!   [N bytes: encrypted payload (AES-256-GCM ciphertext + tag)]
//!
//! The plaintext payload is a simple text format:
//!   USER:<username>\t<password_hash>\t<role>\t<enabled>\t<created_at>\t<updated_at>\n
//!   KEY:<username>\t<key_string>\t<name>\t<role>\t<created_at>\n
//!   SEALED:<true|false>\n

use crate::crypto::aes_gcm::{aes256_gcm_decrypt, aes256_gcm_encrypt};
use crate::crypto::hmac::hmac_sha256;
use crate::crypto::os_random;
use crate::storage::encryption::argon2id::{derive_key, Argon2Params};
use crate::storage::encryption::key::SecureKey;
use crate::storage::engine::page::{Page, PageType, CONTENT_SIZE, HEADER_SIZE};
use crate::storage::engine::pager::Pager;

use super::{ApiKey, AuthError, Role, User};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const VAULT_MAGIC: &[u8; 4] = b"RDVT";
const VAULT_VERSION: u8 = 1;
const VAULT_AAD: &[u8] = b"reddb-vault";

/// Vault header within page content: magic(4) + version(1) + salt(16) + payload_len(4) = 25 bytes.
const VAULT_CONTENT_HEADER_SIZE: usize = 4 + 1 + 16 + 4;

/// AES-256-GCM nonce size.
const NONCE_SIZE: usize = 12;

/// Reserved page IDs for the auth vault within the main .rdb file.
const VAULT_HEADER_PAGE: u32 = 2;
const VAULT_DATA_PAGE: u32 = 3;

/// Usable content per vault page (page size minus the 32-byte page header).
const VAULT_PAGE_CAPACITY: usize = CONTENT_SIZE;

// ---------------------------------------------------------------------------
// KeyPair -- certificate-based vault seal
// ---------------------------------------------------------------------------

/// RedDB cryptographic keypair for vault seal and token signing.
///
/// At bootstrap time a random `master_secret` is generated.  The
/// `certificate` is derived from the master secret via HMAC-SHA256 and
/// given to the admin.  The admin uses the certificate to unseal the
/// vault on subsequent restarts.
///
/// ```text
/// master_secret  = random_bytes(32)                            // lives in vault
/// certificate    = HMAC-SHA256(master_secret, "reddb-certificate-v1")  // admin keeps this
/// vault_key      = Argon2id(certificate, "reddb-vault-seal")   // AES-256-GCM key for vault
/// ```
pub struct KeyPair {
    /// 32-byte master secret (stays encrypted inside the vault).
    pub master_secret: Vec<u8>,
    /// 32-byte certificate derived from master secret (admin keeps this).
    pub certificate: Vec<u8>,
}

impl KeyPair {
    /// Generate a fresh keypair at bootstrap time.
    pub fn generate() -> Self {
        let mut master_secret = vec![0u8; 32];
        os_random::fill_bytes(&mut master_secret).expect("CSPRNG failed during keypair generation");
        let certificate = hmac_sha256(&master_secret, b"reddb-certificate-v1");
        Self {
            master_secret,
            certificate: certificate.to_vec(),
        }
    }

    /// Re-derive a keypair from a known master secret (used when
    /// restoring state from the decrypted vault).
    pub fn from_master_secret(master_secret: Vec<u8>) -> Self {
        let certificate = hmac_sha256(&master_secret, b"reddb-certificate-v1");
        Self {
            master_secret,
            certificate: certificate.to_vec(),
        }
    }

    /// Derive the vault encryption key from a certificate.
    ///
    /// This is the only operation that does NOT require the master
    /// secret -- anyone holding the certificate can unseal the vault.
    pub fn vault_key_from_certificate(certificate: &[u8]) -> SecureKey {
        let key_bytes = derive_key(certificate, b"reddb-vault-seal", &vault_argon2_params());
        SecureKey::new(&key_bytes)
    }

    /// Sign arbitrary data with the master secret (HMAC-SHA256).
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        hmac_sha256(&self.master_secret, data).to_vec()
    }

    /// Verify a signature produced by [`sign`](Self::sign).
    pub fn verify(&self, data: &[u8], signature: &[u8]) -> bool {
        let expected = self.sign(data);
        constant_time_eq(&expected, signature)
    }

    /// Certificate as a hex string (what the admin saves).
    pub fn certificate_hex(&self) -> String {
        hex::encode(&self.certificate)
    }
}

/// Constant-time byte comparison to avoid timing side-channels.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// VaultError
// ---------------------------------------------------------------------------

/// Errors produced by vault operations.
#[derive(Debug)]
pub enum VaultError {
    /// No encryption key available (neither env var nor passphrase).
    NoKey,
    /// Encryption failed.
    Encryption,
    /// Decryption failed (wrong key or corrupt data).
    Decryption,
    /// IO error reading/writing vault pages.
    Io(std::io::Error),
    /// The vault data is structurally corrupt.
    Corrupt(String),
    /// Pager error during page I/O.
    Pager(String),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoKey => write!(
                f,
                "no vault key: set REDDB_CERTIFICATE (or REDDB_VAULT_KEY) or provide a certificate"
            ),
            Self::Encryption => write!(f, "vault encryption failed"),
            Self::Decryption => write!(f, "vault decryption failed (wrong key or corrupt data)"),
            Self::Io(err) => write!(f, "vault I/O error: {err}"),
            Self::Corrupt(msg) => write!(f, "vault corrupt: {msg}"),
            Self::Pager(msg) => write!(f, "vault pager error: {msg}"),
        }
    }
}

impl std::error::Error for VaultError {}

impl From<VaultError> for AuthError {
    fn from(err: VaultError) -> Self {
        AuthError::Internal(err.to_string())
    }
}

// ---------------------------------------------------------------------------
// VaultState
// ---------------------------------------------------------------------------

/// Serializable snapshot of all auth state (users, api keys, bootstrap seal,
/// and the master secret for the certificate-based seal).
pub struct VaultState {
    pub users: Vec<User>,
    /// `(owner_username, api_key)` pairs.
    pub api_keys: Vec<(String, ApiKey)>,
    pub bootstrapped: bool,
    /// The 32-byte master secret stored inside the encrypted vault.
    /// Present after bootstrap; `None` for legacy vaults that pre-date
    /// the certificate seal system.
    pub master_secret: Option<Vec<u8>>,
}

impl VaultState {
    /// Serialize the vault state to the text payload format.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = String::new();

        // Master secret (if present from certificate-based seal).
        if let Some(ref secret) = self.master_secret {
            out.push_str(&format!("MASTER_SECRET:{}\n", hex::encode(secret)));
        }

        // SEALED line.
        out.push_str(&format!("SEALED:{}\n", self.bootstrapped));

        // Users.
        for user in &self.users {
            out.push_str(&format!(
                "USER:{}\t{}\t{}\t{}\t{}\t{}\n",
                user.username,
                user.password_hash,
                user.role.as_str(),
                user.enabled,
                user.created_at,
                user.updated_at,
            ));
        }

        // API keys (with owner username).
        for (username, key) in &self.api_keys {
            out.push_str(&format!(
                "KEY:{}\t{}\t{}\t{}\t{}\n",
                username,
                key.key,
                key.name,
                key.role.as_str(),
                key.created_at,
            ));
        }

        out.into_bytes()
    }

    /// Deserialize the vault state from the text payload format.
    pub fn deserialize(data: &[u8]) -> Result<Self, VaultError> {
        let text = std::str::from_utf8(data)
            .map_err(|_| VaultError::Corrupt("payload is not valid UTF-8".into()))?;

        let mut users = Vec::new();
        let mut api_keys: Vec<(String, ApiKey)> = Vec::new();
        let mut bootstrapped = false;
        let mut master_secret: Option<Vec<u8>> = None;

        for line in text.lines() {
            if line.is_empty() {
                continue;
            }

            if let Some(rest) = line.strip_prefix("MASTER_SECRET:") {
                master_secret = Some(
                    hex::decode(rest)
                        .map_err(|_| VaultError::Corrupt("invalid MASTER_SECRET hex".into()))?,
                );
            } else if let Some(rest) = line.strip_prefix("SEALED:") {
                bootstrapped = rest == "true";
            } else if let Some(rest) = line.strip_prefix("USER:") {
                let parts: Vec<&str> = rest.split('\t').collect();
                if parts.len() != 6 {
                    return Err(VaultError::Corrupt(format!(
                        "USER line has {} fields, expected 6",
                        parts.len()
                    )));
                }
                let role = Role::from_str(parts[2])
                    .ok_or_else(|| VaultError::Corrupt(format!("unknown role: {}", parts[2])))?;
                let enabled = parts[3] == "true";
                let created_at: u128 = parts[4]
                    .parse()
                    .map_err(|_| VaultError::Corrupt("invalid created_at".into()))?;
                let updated_at: u128 = parts[5]
                    .parse()
                    .map_err(|_| VaultError::Corrupt("invalid updated_at".into()))?;

                users.push(User {
                    username: parts[0].to_string(),
                    password_hash: parts[1].to_string(),
                    role,
                    api_keys: Vec::new(), // API keys are attached separately below
                    created_at,
                    updated_at,
                    enabled,
                });
            } else if let Some(rest) = line.strip_prefix("KEY:") {
                let parts: Vec<&str> = rest.split('\t').collect();
                if parts.len() != 5 {
                    return Err(VaultError::Corrupt(format!(
                        "KEY line has {} fields, expected 5",
                        parts.len()
                    )));
                }
                let role = Role::from_str(parts[3])
                    .ok_or_else(|| VaultError::Corrupt(format!("unknown role: {}", parts[3])))?;
                let created_at: u128 = parts[4]
                    .parse()
                    .map_err(|_| VaultError::Corrupt("invalid key created_at".into()))?;

                api_keys.push((
                    parts[0].to_string(),
                    ApiKey {
                        key: parts[1].to_string(),
                        name: parts[2].to_string(),
                        role,
                        created_at,
                    },
                ));
            } else {
                // Unknown line prefix -- skip gracefully for forward compat.
            }
        }

        // Re-attach API keys to their owning users.
        for (owner, key) in &api_keys {
            if let Some(user) = users.iter_mut().find(|u| u.username == *owner) {
                user.api_keys.push(key.clone());
            }
        }

        Ok(Self {
            users,
            api_keys,
            bootstrapped,
            master_secret,
        })
    }
}

// ---------------------------------------------------------------------------
// Vault
// ---------------------------------------------------------------------------

/// Encrypted vault for persisting auth state inside reserved pager pages.
///
/// The vault key is derived from `REDDB_VAULT_KEY` env var or a provided
/// passphrase.  A random salt is generated on first write and persisted
/// inside the vault page so that re-opening with the same passphrase
/// produces the same derived key.
pub struct Vault {
    key: SecureKey,
    salt: [u8; 16],
}

/// Argon2id parameters tuned for vault key derivation.
/// Lighter than the default (16 MB vs 64 MB) so vault open is quick.
fn vault_argon2_params() -> Argon2Params {
    Argon2Params {
        m_cost: 16 * 1024, // 16 MB
        t_cost: 3,
        p: 1,
        tag_len: 32,
    }
}

impl Vault {
    /// Open or prepare a vault backed by reserved pager pages.
    ///
    /// Key derivation: `REDDB_VAULT_KEY` env var takes priority, then
    /// the `passphrase` argument.  If neither is set, returns `NoKey`.
    ///
    /// If vault pages already exist in the pager, the salt is read from
    /// the existing page content.  Otherwise a fresh salt is generated
    /// and will be written on the first `save()` call.
    pub fn open(pager: &Pager, passphrase: Option<&str>) -> Result<Self, VaultError> {
        // Try certificate-based opening first (REDDB_CERTIFICATE env var).
        if let Ok(cert_hex) = std::env::var("REDDB_CERTIFICATE") {
            return Self::with_certificate(pager, &cert_hex);
        }

        // Resolve passphrase: env var > argument.
        let passphrase_str = std::env::var("REDDB_VAULT_KEY")
            .ok()
            .or_else(|| passphrase.map(|s| s.to_string()))
            .ok_or(VaultError::NoKey)?;

        // Try to read the salt from an existing vault page.
        let salt = match read_vault_salt_from_pager(pager) {
            Ok(s) => s,
            Err(_) => {
                // No vault pages yet -- generate a fresh salt.
                let mut salt = [0u8; 16];
                let mut buf = [0u8; 16];
                os_random::fill_bytes(&mut buf)
                    .map_err(|e| VaultError::Corrupt(format!("CSPRNG failed: {e}")))?;
                salt.copy_from_slice(&buf);
                salt
            }
        };

        let key_bytes = derive_key(passphrase_str.as_bytes(), &salt, &vault_argon2_params());
        let key = SecureKey::new(&key_bytes);

        Ok(Self { key, salt })
    }

    /// Open a vault using a certificate hex string (from bootstrap).
    ///
    /// The certificate is used to derive the vault encryption key via
    /// Argon2id.  This is the primary unseal mechanism introduced by the
    /// certificate-based seal system.
    pub fn with_certificate(pager: &Pager, certificate_hex: &str) -> Result<Self, VaultError> {
        let certificate = hex::decode(certificate_hex).map_err(|_| VaultError::NoKey)?;

        let key = KeyPair::vault_key_from_certificate(&certificate);

        // Try to read the salt from an existing vault page.
        let salt = match read_vault_salt_from_pager(pager) {
            Ok(s) => s,
            Err(_) => {
                // No vault pages yet -- generate a fresh salt.
                let mut s = [0u8; 16];
                os_random::fill_bytes(&mut s)
                    .map_err(|e| VaultError::Corrupt(format!("CSPRNG failed: {e}")))?;
                s
            }
        };

        Ok(Self { key, salt })
    }

    /// Open a vault from environment variables.
    ///
    /// Precedence: `REDDB_CERTIFICATE` (primary) > `REDDB_VAULT_KEY` (fallback/deprecated).
    pub fn from_env(pager: &Pager) -> Result<Self, VaultError> {
        if let Ok(cert_hex) = std::env::var("REDDB_CERTIFICATE") {
            return Self::with_certificate(pager, &cert_hex);
        }
        if let Ok(passphrase) = std::env::var("REDDB_VAULT_KEY") {
            return Self::open_with_passphrase(pager, &passphrase);
        }
        Err(VaultError::NoKey)
    }

    /// Open a vault with an explicit passphrase string (no env vars).
    fn open_with_passphrase(pager: &Pager, passphrase: &str) -> Result<Self, VaultError> {
        let salt = match read_vault_salt_from_pager(pager) {
            Ok(s) => s,
            Err(_) => {
                let mut s = [0u8; 16];
                os_random::fill_bytes(&mut s)
                    .map_err(|e| VaultError::Corrupt(format!("CSPRNG failed: {e}")))?;
                s
            }
        };

        let key_bytes = derive_key(passphrase.as_bytes(), &salt, &vault_argon2_params());
        let key = SecureKey::new(&key_bytes);
        Ok(Self { key, salt })
    }

    /// Create a vault keyed by a certificate (raw bytes, not hex).
    ///
    /// Used during bootstrap when the certificate is freshly generated
    /// and not yet hex-encoded.
    pub fn with_certificate_bytes(pager: &Pager, certificate: &[u8]) -> Result<Self, VaultError> {
        let key = KeyPair::vault_key_from_certificate(certificate);

        let salt = match read_vault_salt_from_pager(pager) {
            Ok(s) => s,
            Err(_) => {
                let mut s = [0u8; 16];
                os_random::fill_bytes(&mut s)
                    .map_err(|e| VaultError::Corrupt(format!("CSPRNG failed: {e}")))?;
                s
            }
        };

        Ok(Self { key, salt })
    }

    /// Save the given auth state to the encrypted vault pages.
    pub fn save(&self, pager: &Pager, state: &VaultState) -> Result<(), VaultError> {
        let plaintext = state.serialize();

        // Generate a fresh nonce for every write.
        let mut nonce = [0u8; NONCE_SIZE];
        os_random::fill_bytes(&mut nonce)
            .map_err(|e| VaultError::Corrupt(format!("CSPRNG failed: {e}")))?;

        // Encrypt.
        let key_bytes: &[u8] = self.key.as_bytes();
        let key_arr: &[u8; 32] = key_bytes.try_into().map_err(|_| VaultError::Encryption)?;
        let ciphertext = aes256_gcm_encrypt(key_arr, &nonce, VAULT_AAD, &plaintext);

        // Assemble vault content: magic + version + salt + payload_len + nonce + ciphertext
        let payload_len = (NONCE_SIZE + ciphertext.len()) as u32;
        let total_content_len = VAULT_CONTENT_HEADER_SIZE + NONCE_SIZE + ciphertext.len();

        let mut content = Vec::with_capacity(total_content_len);
        content.extend_from_slice(VAULT_MAGIC);
        content.push(VAULT_VERSION);
        content.extend_from_slice(&self.salt);
        content.extend_from_slice(&payload_len.to_le_bytes());
        content.extend_from_slice(&nonce);
        content.extend_from_slice(&ciphertext);

        // Split content across reserved vault pages (VAULT_HEADER_PAGE and
        // optionally VAULT_DATA_PAGE if it overflows one page).
        let first_chunk_len = content.len().min(VAULT_PAGE_CAPACITY);
        self.write_vault_page(pager, VAULT_HEADER_PAGE, &content[..first_chunk_len])?;

        if content.len() > VAULT_PAGE_CAPACITY {
            self.write_vault_page(pager, VAULT_DATA_PAGE, &content[VAULT_PAGE_CAPACITY..])?;
        }

        // Flush vault pages to disk immediately so auth state survives crashes.
        pager
            .flush()
            .map_err(|e| VaultError::Pager(e.to_string()))?;

        Ok(())
    }

    /// Load auth state from the encrypted vault pages.
    ///
    /// Returns `Ok(None)` if the vault pages do not exist yet.
    pub fn load(&self, pager: &Pager) -> Result<Option<VaultState>, VaultError> {
        // Read the first vault page.
        let page = match pager.read_page_no_checksum(VAULT_HEADER_PAGE) {
            Ok(p) => p,
            Err(_) => return Ok(None), // No vault pages yet.
        };

        let page_content = page.content();

        // Check magic to determine if this page is actually a vault page.
        if page_content.len() < VAULT_CONTENT_HEADER_SIZE {
            return Ok(None);
        }
        if &page_content[0..4] != VAULT_MAGIC {
            return Ok(None); // Not a vault page (page exists but was never written).
        }

        // Validate version.
        if page_content[4] != VAULT_VERSION {
            return Err(VaultError::Corrupt(format!(
                "unsupported vault version: {}",
                page_content[4]
            )));
        }

        // Salt is at bytes 5..21 (already read during open).

        // Payload length.
        let payload_len = u32::from_le_bytes(
            page_content[21..25]
                .try_into()
                .map_err(|_| VaultError::Corrupt("bad payload length bytes".into()))?,
        ) as usize;

        let expected_total = VAULT_CONTENT_HEADER_SIZE + payload_len;

        // Reassemble content from one or two pages.
        let mut content = Vec::with_capacity(expected_total);
        // First page contributes up to VAULT_PAGE_CAPACITY bytes of vault content.
        let first_avail = page_content.len().min(VAULT_PAGE_CAPACITY);
        content.extend_from_slice(&page_content[..first_avail]);

        if expected_total > VAULT_PAGE_CAPACITY {
            // Need the continuation page.
            let data_page = pager
                .read_page_no_checksum(VAULT_DATA_PAGE)
                .map_err(|e| VaultError::Pager(format!("vault data page read: {e}")))?;
            let data_content = data_page.content();
            let needed = expected_total - VAULT_PAGE_CAPACITY;
            let avail = data_content.len().min(needed);
            content.extend_from_slice(&data_content[..avail]);
        }

        if content.len() < expected_total {
            return Err(VaultError::Corrupt(format!(
                "vault truncated: expected {} bytes, got {}",
                expected_total,
                content.len()
            )));
        }

        if payload_len < NONCE_SIZE {
            return Err(VaultError::Corrupt("payload too short for nonce".into()));
        }

        // Extract nonce and ciphertext from the reassembled content.
        let nonce_start = VAULT_CONTENT_HEADER_SIZE;
        let nonce: [u8; NONCE_SIZE] = content[nonce_start..nonce_start + NONCE_SIZE]
            .try_into()
            .map_err(|_| VaultError::Corrupt("bad nonce".into()))?;

        let ciphertext =
            &content[nonce_start + NONCE_SIZE..VAULT_CONTENT_HEADER_SIZE + payload_len];

        // Decrypt.
        let key_bytes: &[u8] = self.key.as_bytes();
        let key_arr: &[u8; 32] = key_bytes.try_into().map_err(|_| VaultError::Decryption)?;
        let plaintext = aes256_gcm_decrypt(key_arr, &nonce, VAULT_AAD, ciphertext)
            .map_err(|_| VaultError::Decryption)?;

        let state = VaultState::deserialize(&plaintext)?;
        Ok(Some(state))
    }

    /// Write vault content into a reserved page via the pager.
    fn write_vault_page(
        &self,
        pager: &Pager,
        page_id: u32,
        content: &[u8],
    ) -> Result<(), VaultError> {
        let mut page = Page::new(PageType::Vault, page_id);

        // Write content into the page's content area (after the 32-byte header).
        let page_bytes = page.as_bytes_mut();
        let copy_len = content.len().min(VAULT_PAGE_CAPACITY);
        page_bytes[HEADER_SIZE..HEADER_SIZE + copy_len].copy_from_slice(&content[..copy_len]);

        // Use write_page_no_checksum because vault pages have their own
        // integrity protection (AES-GCM authentication tag).
        pager
            .write_page_no_checksum(page_id, page)
            .map_err(|e| VaultError::Pager(e.to_string()))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read the 16-byte salt from an existing vault page in the pager.
fn read_vault_salt_from_pager(pager: &Pager) -> Result<[u8; 16], VaultError> {
    let page = pager
        .read_page_no_checksum(VAULT_HEADER_PAGE)
        .map_err(|e| VaultError::Pager(format!("vault page read: {e}")))?;

    let content = page.content();
    if content.len() < VAULT_CONTENT_HEADER_SIZE {
        return Err(VaultError::Corrupt("vault page too short".into()));
    }
    if &content[0..4] != VAULT_MAGIC {
        return Err(VaultError::Corrupt("bad magic bytes".into()));
    }

    let mut salt = [0u8; 16];
    salt.copy_from_slice(&content[5..21]);
    Ok(salt)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{now_ms, ApiKey, Role, User};
    use crate::storage::engine::pager::PagerConfig;

    fn sample_state() -> VaultState {
        let now = now_ms();
        VaultState {
            users: vec![
                User {
                    username: "alice".into(),
                    password_hash: "argon2id$aabbccdd$eeff0011".into(),
                    role: Role::Admin,
                    api_keys: vec![ApiKey {
                        key: "rk_abc123".into(),
                        name: "ci-token".into(),
                        role: Role::Write,
                        created_at: now,
                    }],
                    created_at: now,
                    updated_at: now,
                    enabled: true,
                },
                User {
                    username: "bob".into(),
                    password_hash: "argon2id$11223344$55667788".into(),
                    role: Role::Read,
                    api_keys: vec![],
                    created_at: now,
                    updated_at: now,
                    enabled: false,
                },
            ],
            api_keys: vec![(
                "alice".into(),
                ApiKey {
                    key: "rk_abc123".into(),
                    name: "ci-token".into(),
                    role: Role::Write,
                    created_at: now,
                },
            )],
            bootstrapped: true,
            master_secret: None,
        }
    }

    /// Helper to create a temporary pager for testing.
    fn temp_pager() -> (Pager, std::path::PathBuf) {
        let tmp_dir = std::env::temp_dir().join(format!("reddb_vault_test_{}", now_ms()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let db_path = tmp_dir.join("test.rdb");
        let pager = Pager::open(&db_path, PagerConfig::default()).unwrap();
        (pager, tmp_dir)
    }

    #[test]
    fn test_vault_state_serialize_deserialize_roundtrip() {
        let state = sample_state();
        let serialized = state.serialize();
        let text = std::str::from_utf8(&serialized).unwrap();

        // Verify text format contains expected markers.
        assert!(text.contains("SEALED:true"));
        assert!(text.contains("USER:alice\t"));
        assert!(text.contains("USER:bob\t"));
        assert!(text.contains("KEY:alice\trk_abc123\t"));

        // Deserialize and verify.
        let restored = VaultState::deserialize(&serialized).unwrap();
        assert!(restored.bootstrapped);
        assert_eq!(restored.users.len(), 2);

        let alice = restored
            .users
            .iter()
            .find(|u| u.username == "alice")
            .unwrap();
        assert_eq!(alice.role, Role::Admin);
        assert!(alice.enabled);
        assert_eq!(alice.password_hash, "argon2id$aabbccdd$eeff0011");
        assert_eq!(alice.api_keys.len(), 1);
        assert_eq!(alice.api_keys[0].key, "rk_abc123");
        assert_eq!(alice.api_keys[0].name, "ci-token");
        assert_eq!(alice.api_keys[0].role, Role::Write);

        let bob = restored.users.iter().find(|u| u.username == "bob").unwrap();
        assert_eq!(bob.role, Role::Read);
        assert!(!bob.enabled);
        assert!(bob.api_keys.is_empty());

        assert_eq!(restored.api_keys.len(), 1);
        assert_eq!(restored.api_keys[0].0, "alice");
    }

    #[test]
    fn test_vault_state_empty() {
        let state = VaultState {
            users: vec![],
            api_keys: vec![],
            bootstrapped: false,
            master_secret: None,
        };
        let serialized = state.serialize();
        let restored = VaultState::deserialize(&serialized).unwrap();
        assert!(!restored.bootstrapped);
        assert!(restored.users.is_empty());
        assert!(restored.api_keys.is_empty());
    }

    #[test]
    fn test_vault_state_deserialize_invalid_utf8() {
        let bad_data = vec![0xFF, 0xFE, 0xFD];
        let result = VaultState::deserialize(&bad_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_vault_state_deserialize_bad_user_line() {
        let bad = b"USER:only_two\tfields\n";
        let result = VaultState::deserialize(bad);
        assert!(result.is_err());
    }

    #[test]
    fn test_vault_state_deserialize_bad_key_line() {
        let bad = b"KEY:too\tfew\n";
        let result = VaultState::deserialize(bad);
        assert!(result.is_err());
    }

    #[test]
    fn test_vault_state_deserialize_unknown_line_skipped() {
        let data = b"SEALED:false\nFUTURE:some_data\n";
        let result = VaultState::deserialize(data).unwrap();
        assert!(!result.bootstrapped);
    }

    #[test]
    fn test_vault_pager_save_load_roundtrip() {
        let (pager, tmp_dir) = temp_pager();

        let vault = Vault::open(&pager, Some("test-passphrase-42")).unwrap();

        // Initially no vault pages.
        let loaded = vault.load(&pager).unwrap();
        assert!(loaded.is_none());

        // Save state.
        let state = sample_state();
        vault.save(&pager, &state).unwrap();

        // Load back.
        let restored = vault.load(&pager).unwrap().unwrap();
        assert!(restored.bootstrapped);
        assert_eq!(restored.users.len(), 2);
        assert_eq!(restored.api_keys.len(), 1);

        let alice = restored
            .users
            .iter()
            .find(|u| u.username == "alice")
            .unwrap();
        assert_eq!(alice.role, Role::Admin);
        assert_eq!(alice.api_keys.len(), 1);

        // Re-open vault with same key and load again (salt read from page).
        let vault2 = Vault::open(&pager, Some("test-passphrase-42")).unwrap();
        let restored2 = vault2.load(&pager).unwrap().unwrap();
        assert!(restored2.bootstrapped);
        assert_eq!(restored2.users.len(), 2);

        // Clean up.
        drop(pager);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_vault_wrong_key_fails_decryption() {
        let (pager, tmp_dir) = temp_pager();

        // Save with one key.
        let vault = Vault::open(&pager, Some("correct-key")).unwrap();
        let state = VaultState {
            users: vec![],
            api_keys: vec![],
            bootstrapped: true,
            master_secret: None,
        };
        vault.save(&pager, &state).unwrap();

        // Try to load with a different key.
        let vault2 = Vault::open(&pager, Some("wrong-key")).unwrap();
        let result = vault2.load(&pager);

        assert!(result.is_err());

        // Clean up.
        drop(pager);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_vault_no_key_error() {
        let (pager, tmp_dir) = temp_pager();

        let result = Vault::open(&pager, None);
        // If REDDB_VAULT_KEY or REDDB_CERTIFICATE happens to be set by another
        // test, passphrase=None means we rely on env var.  Without either, it
        // should be NoKey.
        let has_env_key =
            std::env::var("REDDB_VAULT_KEY").is_ok() || std::env::var("REDDB_CERTIFICATE").is_ok();
        match has_env_key {
            true => {
                // Env var is set (by another test); open will succeed.
                assert!(result.is_ok());
            }
            false => {
                assert!(matches!(result, Err(VaultError::NoKey)));
            }
        }

        // Clean up.
        drop(pager);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_vault_passphrase_argument() {
        let (pager, tmp_dir) = temp_pager();

        // Open with passphrase argument.
        let vault = Vault::open(&pager, Some("my-passphrase")).unwrap();
        let state = VaultState {
            users: vec![],
            api_keys: vec![],
            bootstrapped: false,
            master_secret: None,
        };
        vault.save(&pager, &state).unwrap();

        // Re-open with same passphrase.
        let vault2 = Vault::open(&pager, Some("my-passphrase")).unwrap();
        let loaded = vault2.load(&pager).unwrap().unwrap();
        assert!(!loaded.bootstrapped);

        drop(pager);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    // ---------------------------------------------------------------
    // KeyPair and certificate-based seal tests
    // ---------------------------------------------------------------

    #[test]
    fn test_keypair_generate_deterministic_certificate() {
        let kp = KeyPair::generate();
        assert_eq!(kp.master_secret.len(), 32);
        assert_eq!(kp.certificate.len(), 32);

        // Re-deriving from the same master secret gives the same certificate.
        let kp2 = KeyPair::from_master_secret(kp.master_secret.clone());
        assert_eq!(kp.certificate, kp2.certificate);
    }

    #[test]
    fn test_keypair_sign_verify() {
        let kp = KeyPair::generate();
        let data = b"session:abc123";
        let sig = kp.sign(data);
        assert!(kp.verify(data, &sig));

        // Wrong data fails.
        assert!(!kp.verify(b"session:wrong", &sig));

        // Wrong signature fails.
        let mut bad_sig = sig.clone();
        bad_sig[0] ^= 0xFF;
        assert!(!kp.verify(data, &bad_sig));
    }

    #[test]
    fn test_keypair_certificate_hex() {
        let kp = KeyPair::generate();
        let hex_str = kp.certificate_hex();
        assert_eq!(hex_str.len(), 64); // 32 bytes = 64 hex chars
        let decoded = hex::decode(&hex_str).unwrap();
        assert_eq!(decoded, kp.certificate);
    }

    #[test]
    fn test_vault_certificate_seal_roundtrip() {
        let (pager, tmp_dir) = temp_pager();

        // Generate a keypair and create a vault sealed by its certificate.
        let kp = KeyPair::generate();
        let vault = Vault::with_certificate_bytes(&pager, &kp.certificate).unwrap();

        // Save state including the master secret.
        let state = VaultState {
            users: vec![],
            api_keys: vec![],
            bootstrapped: true,
            master_secret: Some(kp.master_secret.clone()),
        };
        vault.save(&pager, &state).unwrap();

        // Re-open using the certificate hex string (simulates admin unseal).
        let vault2 = Vault::with_certificate(&pager, &kp.certificate_hex()).unwrap();
        let loaded = vault2.load(&pager).unwrap().unwrap();
        assert!(loaded.bootstrapped);
        assert_eq!(loaded.master_secret, Some(kp.master_secret.clone()));

        // Verify the master secret can reconstruct the same keypair.
        let kp2 = KeyPair::from_master_secret(loaded.master_secret.unwrap());
        assert_eq!(kp.certificate, kp2.certificate);

        drop(pager);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_vault_certificate_wrong_cert_fails() {
        let (pager, tmp_dir) = temp_pager();

        // Seal with one keypair.
        let kp = KeyPair::generate();
        let vault = Vault::with_certificate_bytes(&pager, &kp.certificate).unwrap();
        let state = VaultState {
            users: vec![],
            api_keys: vec![],
            bootstrapped: true,
            master_secret: Some(kp.master_secret.clone()),
        };
        vault.save(&pager, &state).unwrap();

        // Try to unseal with a different certificate.
        let kp2 = KeyPair::generate();
        let vault2 = Vault::with_certificate_bytes(&pager, &kp2.certificate).unwrap();
        let result = vault2.load(&pager);
        assert!(result.is_err());

        drop(pager);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_vault_state_master_secret_serialization() {
        let secret = vec![0xAA; 32];
        let state = VaultState {
            users: vec![],
            api_keys: vec![],
            bootstrapped: true,
            master_secret: Some(secret.clone()),
        };
        let serialized = state.serialize();
        let text = std::str::from_utf8(&serialized).unwrap();
        assert!(text.contains("MASTER_SECRET:"));
        assert!(text.contains(&hex::encode(&secret)));

        let restored = VaultState::deserialize(&serialized).unwrap();
        assert_eq!(restored.master_secret, Some(secret));
        assert!(restored.bootstrapped);
    }

    #[test]
    fn test_vault_state_no_master_secret_backward_compat() {
        // Legacy vault format without MASTER_SECRET line.
        let data = b"SEALED:true\n";
        let restored = VaultState::deserialize(data).unwrap();
        assert!(restored.master_secret.is_none());
        assert!(restored.bootstrapped);
    }

    #[test]
    fn test_constant_time_eq_function() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"short", b"longer"));
        assert!(constant_time_eq(b"", b""));
    }
}
