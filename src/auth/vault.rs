//! Encrypted vault for auth state persistence.
//!
//! Stores users and API keys in **a chained set of reserved pages** inside
//! the main `.rdb` database file instead of a separate `_vault.rdb` file.
//! The vault header lives at a fixed page id (`VAULT_HEADER_PAGE = 2`)
//! using `PageType::Vault` and points to a chain of overflow pages
//! allocated dynamically as the payload grows. The contents are
//! encrypted with a SEPARATE key derived from `REDDB_VAULT_KEY`
//! (or, preferably, a certificate via `REDDB_CERTIFICATE`).
//!
//! # On-disk format (v2)
//!
//! Header page content (inside the 4 KiB page, after the 32-byte
//! page header — i.e. the bytes returned by `page.content()`):
//!
//! ```text
//!   [ 4 bytes: magic "RDVT"             ]
//!   [ 1 byte : version = 2              ]
//!   [16 bytes: salt (key derivation)    ]
//!   [ 4 bytes: total_payload_len u32 LE ]   // == NONCE_SIZE + ciphertext_len
//!   [12 bytes: nonce                    ]
//!   [ 4 bytes: chain_count u32 LE       ]   // number of data pages
//!   [ 4 bytes: first_data_page_id u32 LE]   // 0 if no chain (single page)
//!   [ N bytes: ciphertext fragment      ]   // first slice of the GCM ciphertext+tag
//! ```
//!
//! Each data page (also `PageType::Vault`, allocated by the pager):
//!
//! ```text
//!   [ 4 bytes: magic "RDVD"          ]
//!   [ 4 bytes: next_page_id u32 LE   ]   // 0 if this is the last
//!   [ N bytes: ciphertext fragment   ]
//! ```
//!
//! The total ciphertext (with the 16-byte AES-GCM authentication tag at
//! the end) is the concatenation of every fragment in chain order. We
//! only call `aes256_gcm_decrypt` after the whole chain is reassembled,
//! so a partial / corrupted chain produces a clean `Decryption` error
//! instead of leaking unauthenticated bytes.
//!
//! # Plaintext payload format
//!
//! Newline-separated records:
//!
//! ```text
//!   MASTER_SECRET:<hex>\n
//!   SEALED:<true|false>\n
//!   USER:<username>\t<password_hash>\t<role>\t<enabled>\t<created_at>\t<updated_at>\t<scram_verifier?>\n
//!   KEY:<username>\t<key_string>\t<name>\t<role>\t<created_at>\n
//!   KV:<key>\t<hex_value>\n
//! ```
//!
//! # Crash safety
//!
//! Save order: encrypt → write all data pages first → finally rewrite
//! the header page in place. The header page is the commit point: its
//! `chain_count` + `first_data_page_id` describe whichever chain is
//! actually usable on the next open. Crashing mid-save leaves the
//! previous header (and its chain) untouched, so the old vault remains
//! readable.
//!
//! When the new payload is *smaller* than the existing one, the surplus
//! pages from the old chain are returned to the freelist via
//! `Pager::free_page` so the file does not grow unbounded.

use crate::crypto::aes_gcm::{aes256_gcm_decrypt, aes256_gcm_encrypt};
use crate::crypto::hmac::hmac_sha256;
use crate::crypto::os_random;
use crate::storage::encryption::argon2id::{derive_key, Argon2Params};
use crate::storage::encryption::key::SecureKey;
use crate::storage::engine::page::{Page, PageType, CONTENT_SIZE, HEADER_SIZE};
use crate::storage::engine::pager::Pager;

use super::{ApiKey, AuthError, Role, User, UserId};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const VAULT_MAGIC: &[u8; 4] = b"RDVT";
const VAULT_DATA_MAGIC: &[u8; 4] = b"RDVD";

/// Current on-disk vault format. v1 was the legacy fixed two-page
/// format (pages 2 + 3); v2 introduces the dynamic chain.
const VAULT_VERSION: u8 = 2;

/// Last legacy version. Pre-1.0 we refuse to migrate it — operators
/// re-bootstrap with `red bootstrap` to upgrade.
const VAULT_LEGACY_VERSION: u8 = 1;

const VAULT_AAD: &[u8] = b"reddb-vault";

/// Header content layout sizes (after the page's own 32-byte header).
const VAULT_MAGIC_SIZE: usize = 4;
const VAULT_VERSION_SIZE: usize = 1;
const VAULT_SALT_SIZE: usize = 16;
const VAULT_PAYLOAD_LEN_SIZE: usize = 4;
const VAULT_CHAIN_COUNT_SIZE: usize = 4;
const VAULT_FIRST_PAGE_ID_SIZE: usize = 4;

/// AES-256-GCM nonce size.
const NONCE_SIZE: usize = 12;

/// Header preamble (everything up to and including `total_payload_len`).
const VAULT_HEADER_PREAMBLE_SIZE: usize =
    VAULT_MAGIC_SIZE + VAULT_VERSION_SIZE + VAULT_SALT_SIZE + VAULT_PAYLOAD_LEN_SIZE; // 25

/// Total fixed metadata at the start of the header page's content area:
/// preamble + nonce + chain_count + first_data_page_id.
const VAULT_HEADER_META_SIZE: usize =
    VAULT_HEADER_PREAMBLE_SIZE + NONCE_SIZE + VAULT_CHAIN_COUNT_SIZE + VAULT_FIRST_PAGE_ID_SIZE; // 25 + 12 + 4 + 4 = 45

/// Fixed prefix on every data page (magic + next_page_id).
const VAULT_DATA_PREFIX_SIZE: usize = VAULT_MAGIC_SIZE + 4; // 8 bytes

/// Reserved page id for the vault header (entry point of the chain).
/// Data pages are allocated dynamically and may live anywhere in the file.
const VAULT_HEADER_PAGE: u32 = 2;

/// Bytes of ciphertext that fit alongside the metadata in the header page.
/// CONTENT_SIZE (4064) − VAULT_HEADER_META_SIZE (45) = 4019.
const VAULT_HEADER_CIPHER_CAPACITY: usize = CONTENT_SIZE - VAULT_HEADER_META_SIZE;

/// Bytes of ciphertext that fit in a single overflow page.
/// CONTENT_SIZE (4064) − VAULT_DATA_PREFIX_SIZE (8) = 4056.
const VAULT_DATA_CIPHER_CAPACITY: usize = CONTENT_SIZE - VAULT_DATA_PREFIX_SIZE;

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
/// the master secret for the certificate-based seal, and a key-value store
/// for arbitrary encrypted secrets).
#[derive(Debug, Default)]
pub struct VaultState {
    pub users: Vec<User>,
    /// `(owner UserId, api_key)` pairs. The owner carries tenant scope
    /// so an API key under `(acme, alice)` reattaches to the correct
    /// user when a same-named user exists in another tenant.
    pub api_keys: Vec<(UserId, ApiKey)>,
    pub bootstrapped: bool,
    /// The 32-byte master secret stored inside the encrypted vault.
    /// Present after bootstrap; `None` for legacy vaults that pre-date
    /// the certificate seal system.
    pub master_secret: Option<Vec<u8>>,
    /// Arbitrary encrypted key-value store for secrets.
    /// Keys use dot-notation with `red.secret.*` prefix (e.g., "red.secret.aes_key").
    /// Values are hex-encoded bytes or UTF-8 strings.
    pub kv: std::collections::HashMap<String, String>,
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
        //
        // USER line tabs: <username> <pw_hash> <role> <enabled>
        // <created_at> <updated_at> <scram_verifier?> <tenant_id?>
        //
        // Field counts accepted on read:
        //   * 7 fields — pre-tenant USER line (any prior v2 vault
        //     written before tenant scoping landed).
        //   * 8 fields — current USER line. The 8th field is the
        //     tenant id; empty string = platform tenant (`None`).
        //
        // Verifier encoding: `<salt_hex>:<iter>:<stored_hex>:<server_hex>`.
        for user in &self.users {
            let scram_field = match &user.scram_verifier {
                Some(v) => format!(
                    "{}:{}:{}:{}",
                    hex::encode(&v.salt),
                    v.iter,
                    hex::encode(v.stored_key),
                    hex::encode(v.server_key),
                ),
                None => String::new(),
            };
            let tenant_field = user.tenant_id.clone().unwrap_or_default();
            out.push_str(&format!(
                "USER:{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                user.username,
                user.password_hash,
                user.role.as_str(),
                user.enabled,
                user.created_at,
                user.updated_at,
                scram_field,
                tenant_field,
            ));
        }

        // API keys: `KEY:<username>\t<key>\t<name>\t<role>\t<created_at>\t<tenant_id?>`.
        // The 6th tenant field is empty for platform users and disambiguates
        // owners when the same username appears under multiple tenants.
        for (owner, key) in &self.api_keys {
            let tenant_field = owner.tenant.clone().unwrap_or_default();
            out.push_str(&format!(
                "KEY:{}\t{}\t{}\t{}\t{}\t{}\n",
                owner.username,
                key.key,
                key.name,
                key.role.as_str(),
                key.created_at,
                tenant_field,
            ));
        }

        // KV entries (hex-encoded values to avoid newline/tab collisions).
        for (k, v) in &self.kv {
            out.push_str(&format!("KV:{}\t{}\n", k, hex::encode(v.as_bytes())));
        }

        out.into_bytes()
    }

    /// Deserialize the vault state from the text payload format.
    pub fn deserialize(data: &[u8]) -> Result<Self, VaultError> {
        let text = std::str::from_utf8(data)
            .map_err(|_| VaultError::Corrupt("payload is not valid UTF-8".into()))?;

        let mut users = Vec::new();
        let mut api_keys: Vec<(UserId, ApiKey)> = Vec::new();
        let mut bootstrapped = false;
        let mut master_secret: Option<Vec<u8>> = None;
        let mut kv: std::collections::HashMap<String, String> = std::collections::HashMap::new();

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
                // 7 fields = pre-tenant v2 USER line; 8 fields = with
                // tenant id appended. Anything else is corrupt.
                if parts.len() != 7 && parts.len() != 8 {
                    return Err(VaultError::Corrupt(format!(
                        "USER line has {} fields, expected 7 or 8",
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
                let scram_verifier = parts
                    .get(6)
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(parse_scram_field)
                    .transpose()?;
                let tenant_id = parts
                    .get(7)
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());

                users.push(User {
                    username: parts[0].to_string(),
                    tenant_id,
                    password_hash: parts[1].to_string(),
                    scram_verifier,
                    role,
                    api_keys: Vec::new(), // API keys are attached separately below
                    created_at,
                    updated_at,
                    enabled,
                });
            } else if let Some(rest) = line.strip_prefix("KEY:") {
                let parts: Vec<&str> = rest.split('\t').collect();
                // 5 fields = pre-tenant; 6 fields = with tenant id.
                if parts.len() != 5 && parts.len() != 6 {
                    return Err(VaultError::Corrupt(format!(
                        "KEY line has {} fields, expected 5 or 6",
                        parts.len()
                    )));
                }
                let role = Role::from_str(parts[3])
                    .ok_or_else(|| VaultError::Corrupt(format!("unknown role: {}", parts[3])))?;
                let created_at: u128 = parts[4]
                    .parse()
                    .map_err(|_| VaultError::Corrupt("invalid key created_at".into()))?;
                let tenant_id = parts
                    .get(5)
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());

                api_keys.push((
                    UserId {
                        tenant: tenant_id,
                        username: parts[0].to_string(),
                    },
                    ApiKey {
                        key: parts[1].to_string(),
                        name: parts[2].to_string(),
                        role,
                        created_at,
                    },
                ));
            } else if let Some(rest) = line.strip_prefix("KV:") {
                let parts: Vec<&str> = rest.splitn(2, '\t').collect();
                if parts.len() == 2 {
                    if let Ok(bytes) = hex::decode(parts[1]) {
                        if let Ok(value) = String::from_utf8(bytes) {
                            kv.insert(parts[0].to_string(), value);
                        }
                    }
                }
            } else {
                // Unknown line prefix -- skip gracefully for forward compat.
            }
        }

        // Re-attach API keys to their owning users. Match on the full
        // `(tenant, username)` so a key for `(acme, alice)` doesn't
        // accidentally attach to `(globex, alice)`.
        for (owner, key) in &api_keys {
            if let Some(user) = users
                .iter_mut()
                .find(|u| u.username == owner.username && u.tenant_id == owner.tenant)
            {
                user.api_keys.push(key.clone());
            }
        }

        Ok(Self {
            users,
            api_keys,
            bootstrapped,
            master_secret,
            kv,
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
    ///
    /// Order of operations is the only thing keeping this crash-safe:
    ///   1. Encrypt the serialized state under a fresh nonce.
    ///   2. Allocate (or reuse) the data-page chain and write every
    ///      data page to disk.
    ///   3. Free any surplus pages that the previous chain owned.
    ///   4. Rewrite the header page in place — this is the commit
    ///      point. After it lands, `load()` will follow the new chain.
    /// A crash anywhere before step 4 leaves the existing header (and
    /// its chain) intact, so the previous vault snapshot is still
    /// readable on the next open.
    pub fn save(&self, pager: &Pager, state: &VaultState) -> Result<(), VaultError> {
        let plaintext = state.serialize();

        // Fresh nonce per write — required for AES-GCM.
        let mut nonce = [0u8; NONCE_SIZE];
        os_random::fill_bytes(&mut nonce)
            .map_err(|e| VaultError::Corrupt(format!("CSPRNG failed: {e}")))?;

        let key_bytes: &[u8] = self.key.as_bytes();
        let key_arr: &[u8; 32] = key_bytes.try_into().map_err(|_| VaultError::Encryption)?;
        let ciphertext = aes256_gcm_encrypt(key_arr, &nonce, VAULT_AAD, &plaintext);
        // The 16-byte GCM tag is appended to `ciphertext` already; we
        // treat the whole vector as one opaque blob.

        let cipher_total = ciphertext.len();
        let payload_len = (NONCE_SIZE + cipher_total) as u32; // for legacy field

        // ---- 1. Plan the chain --------------------------------------
        //
        // The header page absorbs the first `VAULT_HEADER_CIPHER_CAPACITY`
        // bytes of ciphertext; everything after spills into a chain of
        // data pages with `VAULT_DATA_CIPHER_CAPACITY` bytes each.
        let header_chunk_len = cipher_total.min(VAULT_HEADER_CIPHER_CAPACITY);
        let overflow = cipher_total.saturating_sub(header_chunk_len);
        let chain_count = overflow.div_ceil(VAULT_DATA_CIPHER_CAPACITY);

        // ---- 2. Reserve VAULT_HEADER_PAGE on a fresh DB -------------
        //
        // The pager hands out ids from `page_count` upward. On a brand
        // new file `page_count == 1` (only the database header at id
        // 0), so without this dance the next call to `allocate_page`
        // would happily return id 1 and then id 2 — colliding with our
        // fixed VAULT_HEADER_PAGE. Burn allocations until `page_count`
        // is past the header so future `allocate_page(Vault)` calls
        // for the data chain return ids >= VAULT_HEADER_PAGE + 1.
        //
        // We pass `PageType::Vault` so anyone scanning page types sees
        // the right tag for these reserved slots; the header-page
        // contents get overwritten below in any case.
        while pager
            .page_count()
            .map_err(|e| VaultError::Pager(e.to_string()))?
            <= VAULT_HEADER_PAGE
        {
            pager
                .allocate_page(PageType::Vault)
                .map_err(|e| VaultError::Pager(format!("reserve vault slot: {e}")))?;
        }

        // ---- 3. Snapshot the previous chain (if any) for later cleanup.
        //
        // We do NOT reuse these ids — overwriting an old data page
        // before the header is rewritten would corrupt the live vault
        // mid-save (the still-valid header would point at a page that
        // now has the *new* ciphertext but the *old* nonce). Allocating
        // fresh pages means the old chain stays byte-identical until
        // the header commit, so `load()` keeps working through any
        // crash before step 7.
        let old_chain = match self.read_existing_chain_ids(pager) {
            Ok(ids) => ids,
            Err(_) => Vec::new(), // No prior vault — fine.
        };

        // ---- 4. Allocate fresh data-page ids for the new chain.
        //
        // The pager pulls from the freelist first, so successive
        // saves recycle ids without growing the file — the old chain
        // is freed at step 6 below and becomes available the *next*
        // time we save.
        let mut new_chain: Vec<u32> = Vec::with_capacity(chain_count);
        for _ in 0..chain_count {
            let page = pager
                .allocate_page(PageType::Vault)
                .map_err(|e| VaultError::Pager(format!("allocate vault data page: {e}")))?;
            new_chain.push(page.page_id());
        }

        // ---- 5. Write data pages. We already know every page id up
        // front (allocated in step 4), so each page can record its
        // successor's id directly — no second pass needed. The header
        // is *not* updated yet, so a crash here leaves `load()`
        // looking at the previous chain (which is still on disk and
        // valid because we did not touch its pages).
        let mut cursor = header_chunk_len;
        for i in 0..chain_count {
            let next_id = if i + 1 < chain_count {
                new_chain[i + 1]
            } else {
                0
            };
            let take = (cipher_total - cursor).min(VAULT_DATA_CIPHER_CAPACITY);
            let frag = &ciphertext[cursor..cursor + take];
            self.write_data_page(pager, new_chain[i], next_id, frag)?;
            cursor += take;
        }
        debug_assert_eq!(cursor, cipher_total, "ciphertext spill accounting mismatch");

        // ---- 6. (Deferred) The old chain pages are freed *after* the
        // header commit so the freelist doesn't hand them back out
        // before we've finished swapping over.

        // ---- 7. Rewrite the header page. This is the commit point —
        // after this write the new chain is authoritative and any
        // future load() will follow it.
        let first_data_page = new_chain.first().copied().unwrap_or(0);
        self.write_header_page(
            pager,
            &nonce,
            payload_len,
            chain_count as u32,
            first_data_page,
            &ciphertext[..header_chunk_len],
        )?;

        // ---- 8. Flush so a process crash after return doesn't lose
        // the write. We flush *before* freeing old pages so the new
        // header is durable on disk before we tell the pager those
        // old ids are reusable.
        pager
            .flush()
            .map_err(|e| VaultError::Pager(e.to_string()))?;

        // ---- 9. Now safe to free the old chain. The freelist update
        // makes those page ids reclaimable on the *next* allocation,
        // which is exactly what we want — the old data is no longer
        // referenced by the (just-flushed) header.
        for &id in old_chain.iter() {
            pager
                .free_page(id)
                .map_err(|e| VaultError::Pager(format!("free old vault page {id}: {e}")))?;
        }

        Ok(())
    }

    /// Load auth state from the encrypted vault pages.
    ///
    /// Returns `Ok(None)` if the vault pages do not exist yet (fresh DB).
    pub fn load(&self, pager: &Pager) -> Result<Option<VaultState>, VaultError> {
        // Header page is the entry point.
        let page = match pager.read_page_no_checksum(VAULT_HEADER_PAGE) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        let page_content = page.content();

        if page_content.len() < VAULT_HEADER_META_SIZE {
            return Ok(None);
        }
        if &page_content[0..VAULT_MAGIC_SIZE] != VAULT_MAGIC {
            return Ok(None); // Slot is reserved but never written.
        }

        let version = page_content[4];
        if version == VAULT_LEGACY_VERSION {
            // Pre-1.0: no migration shim. Fail loudly with operator
            // guidance so this gets surfaced during upgrade and not
            // hidden behind a generic decryption error.
            return Err(VaultError::Corrupt(
                "vault was bootstrapped with the legacy 2-page format \
                 (pre-RedDB v0.3); re-bootstrap with `red bootstrap` to upgrade"
                    .to_string(),
            ));
        }
        if version != VAULT_VERSION {
            return Err(VaultError::Corrupt(format!(
                "unsupported vault version: {} (expected {})",
                version, VAULT_VERSION
            )));
        }

        // Decode header preamble.
        let payload_len = u32::from_le_bytes(
            page_content[21..25]
                .try_into()
                .map_err(|_| VaultError::Corrupt("bad payload length bytes".into()))?,
        ) as usize;

        let nonce_start = VAULT_HEADER_PREAMBLE_SIZE;
        let nonce: [u8; NONCE_SIZE] = page_content[nonce_start..nonce_start + NONCE_SIZE]
            .try_into()
            .map_err(|_| VaultError::Corrupt("bad nonce".into()))?;

        let chain_count_off = nonce_start + NONCE_SIZE;
        let chain_count = u32::from_le_bytes(
            page_content[chain_count_off..chain_count_off + 4]
                .try_into()
                .map_err(|_| VaultError::Corrupt("bad chain_count bytes".into()))?,
        ) as usize;
        let first_id_off = chain_count_off + 4;
        let mut next_id = u32::from_le_bytes(
            page_content[first_id_off..first_id_off + 4]
                .try_into()
                .map_err(|_| VaultError::Corrupt("bad first_data_page_id bytes".into()))?,
        );

        if payload_len < NONCE_SIZE {
            return Err(VaultError::Corrupt("payload too short for nonce".into()));
        }
        let cipher_total = payload_len - NONCE_SIZE;

        // ---- Reassemble ciphertext fragments. ----------------------
        let mut cipher = Vec::with_capacity(cipher_total);
        let header_chunk_len = cipher_total.min(VAULT_HEADER_CIPHER_CAPACITY);
        let header_cipher_start = VAULT_HEADER_META_SIZE;
        cipher.extend_from_slice(
            &page_content[header_cipher_start..header_cipher_start + header_chunk_len],
        );

        // Walk the data-page chain.
        let mut hops = 0usize;
        // Bound the walk: chain_count from the header is the source of
        // truth. We tolerate next_id pointers but trust chain_count to
        // avoid getting trapped in a corrupt loop.
        while cipher.len() < cipher_total {
            if hops >= chain_count {
                return Err(VaultError::Corrupt(format!(
                    "vault chain shorter than declared: {} hops, expected {}",
                    hops, chain_count
                )));
            }
            if next_id == 0 {
                return Err(VaultError::Corrupt(
                    "vault chain ends prematurely (next_id == 0)".to_string(),
                ));
            }

            let dp = pager
                .read_page_no_checksum(next_id)
                .map_err(|e| VaultError::Pager(format!("vault data page {next_id}: {e}")))?;
            let dp_content = dp.content();
            if dp_content.len() < VAULT_DATA_PREFIX_SIZE {
                return Err(VaultError::Corrupt(format!(
                    "vault data page {next_id} truncated"
                )));
            }
            if &dp_content[0..VAULT_MAGIC_SIZE] != VAULT_DATA_MAGIC {
                return Err(VaultError::Corrupt(format!(
                    "vault data page {next_id} has bad magic"
                )));
            }
            let np = u32::from_le_bytes(
                dp_content[VAULT_MAGIC_SIZE..VAULT_MAGIC_SIZE + 4]
                    .try_into()
                    .map_err(|_| VaultError::Corrupt("bad next_page_id bytes".into()))?,
            );
            let take = (cipher_total - cipher.len()).min(VAULT_DATA_CIPHER_CAPACITY);
            let frag_start = VAULT_DATA_PREFIX_SIZE;
            cipher.extend_from_slice(&dp_content[frag_start..frag_start + take]);

            next_id = np;
            hops += 1;
        }

        if cipher.len() != cipher_total {
            return Err(VaultError::Corrupt(format!(
                "vault truncated: expected {} cipher bytes, got {}",
                cipher_total,
                cipher.len()
            )));
        }
        if hops != chain_count {
            return Err(VaultError::Corrupt(format!(
                "vault chain length mismatch: walked {} pages, header says {}",
                hops, chain_count
            )));
        }

        // ---- Decrypt the reassembled blob in one shot. -------------
        let key_bytes: &[u8] = self.key.as_bytes();
        let key_arr: &[u8; 32] = key_bytes.try_into().map_err(|_| VaultError::Decryption)?;
        let plaintext = aes256_gcm_decrypt(key_arr, &nonce, VAULT_AAD, &cipher)
            .map_err(|_| VaultError::Decryption)?;

        let state = VaultState::deserialize(&plaintext)?;
        Ok(Some(state))
    }

    /// Walk the existing chain (if any) and collect the data-page ids,
    /// so `save()` can reuse / free them. Returns an error or empty
    /// vector if the chain isn't intact — callers must treat that as
    /// "no reusable chain" rather than failing the save outright,
    /// because a partially-corrupt chain is exactly the case where we
    /// most want a fresh write to land cleanly.
    fn read_existing_chain_ids(&self, pager: &Pager) -> Result<Vec<u32>, VaultError> {
        let header = pager
            .read_page_no_checksum(VAULT_HEADER_PAGE)
            .map_err(|e| VaultError::Pager(e.to_string()))?;
        let content = header.content();
        if content.len() < VAULT_HEADER_META_SIZE {
            return Ok(Vec::new());
        }
        if &content[0..VAULT_MAGIC_SIZE] != VAULT_MAGIC {
            return Ok(Vec::new());
        }
        let version = content[4];
        if version != VAULT_VERSION {
            // v1 (legacy) had its overflow at fixed page 3; we don't
            // know if that page is "ours" to free. Safer to leak it
            // — the operator is re-bootstrapping anyway.
            return Ok(Vec::new());
        }
        let nonce_start = VAULT_HEADER_PREAMBLE_SIZE;
        let chain_count_off = nonce_start + NONCE_SIZE;
        let chain_count = u32::from_le_bytes(
            content[chain_count_off..chain_count_off + 4]
                .try_into()
                .map_err(|_| VaultError::Corrupt("bad chain_count bytes".into()))?,
        ) as usize;
        let first_id_off = chain_count_off + 4;
        let mut id = u32::from_le_bytes(
            content[first_id_off..first_id_off + 4]
                .try_into()
                .map_err(|_| VaultError::Corrupt("bad first_data_page_id bytes".into()))?,
        );

        let mut out = Vec::with_capacity(chain_count);
        let mut hops = 0usize;
        while id != 0 && hops < chain_count {
            out.push(id);
            // Peek next_id off the data page. Soft-fail on read errors
            // — we'd rather leak a page than refuse to save.
            match pager.read_page_no_checksum(id) {
                Ok(dp) => {
                    let dc = dp.content();
                    if dc.len() < VAULT_DATA_PREFIX_SIZE
                        || &dc[0..VAULT_MAGIC_SIZE] != VAULT_DATA_MAGIC
                    {
                        break;
                    }
                    id = u32::from_le_bytes(
                        dc[VAULT_MAGIC_SIZE..VAULT_MAGIC_SIZE + 4]
                            .try_into()
                            .map_err(|_| VaultError::Corrupt("bad next_id".into()))?,
                    );
                }
                Err(_) => break,
            }
            hops += 1;
        }
        Ok(out)
    }

    /// Write the vault header page (magic + version + chain metadata +
    /// nonce + first ciphertext fragment). This is the commit point —
    /// callers must have flushed every data page first.
    fn write_header_page(
        &self,
        pager: &Pager,
        nonce: &[u8; NONCE_SIZE],
        payload_len: u32,
        chain_count: u32,
        first_data_page_id: u32,
        cipher_fragment: &[u8],
    ) -> Result<(), VaultError> {
        debug_assert!(cipher_fragment.len() <= VAULT_HEADER_CIPHER_CAPACITY);

        let mut page = Page::new(PageType::Vault, VAULT_HEADER_PAGE);
        let bytes = page.as_bytes_mut();
        let mut off = HEADER_SIZE;

        bytes[off..off + VAULT_MAGIC_SIZE].copy_from_slice(VAULT_MAGIC);
        off += VAULT_MAGIC_SIZE;

        bytes[off] = VAULT_VERSION;
        off += VAULT_VERSION_SIZE;

        bytes[off..off + VAULT_SALT_SIZE].copy_from_slice(&self.salt);
        off += VAULT_SALT_SIZE;

        bytes[off..off + 4].copy_from_slice(&payload_len.to_le_bytes());
        off += VAULT_PAYLOAD_LEN_SIZE;

        bytes[off..off + NONCE_SIZE].copy_from_slice(nonce);
        off += NONCE_SIZE;

        bytes[off..off + 4].copy_from_slice(&chain_count.to_le_bytes());
        off += VAULT_CHAIN_COUNT_SIZE;

        bytes[off..off + 4].copy_from_slice(&first_data_page_id.to_le_bytes());
        off += VAULT_FIRST_PAGE_ID_SIZE;

        debug_assert_eq!(off, HEADER_SIZE + VAULT_HEADER_META_SIZE);

        bytes[off..off + cipher_fragment.len()].copy_from_slice(cipher_fragment);

        pager
            .write_page_no_checksum(VAULT_HEADER_PAGE, page)
            .map_err(|e| VaultError::Pager(e.to_string()))?;
        Ok(())
    }

    /// Write a data page (magic + next_page_id + ciphertext fragment).
    fn write_data_page(
        &self,
        pager: &Pager,
        page_id: u32,
        next_page_id: u32,
        cipher_fragment: &[u8],
    ) -> Result<(), VaultError> {
        debug_assert!(cipher_fragment.len() <= VAULT_DATA_CIPHER_CAPACITY);

        let mut page = Page::new(PageType::Vault, page_id);
        let bytes = page.as_bytes_mut();
        let mut off = HEADER_SIZE;

        bytes[off..off + VAULT_MAGIC_SIZE].copy_from_slice(VAULT_DATA_MAGIC);
        off += VAULT_MAGIC_SIZE;

        bytes[off..off + 4].copy_from_slice(&next_page_id.to_le_bytes());
        off += 4;

        bytes[off..off + cipher_fragment.len()].copy_from_slice(cipher_fragment);

        pager
            .write_page_no_checksum(page_id, page)
            .map_err(|e| VaultError::Pager(e.to_string()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decode a SCRAM verifier field of the form
/// `<salt_hex>:<iter>:<stored_hex>:<server_hex>` into a `ScramVerifier`.
fn parse_scram_field(field: &str) -> Result<crate::auth::scram::ScramVerifier, VaultError> {
    let parts: Vec<&str> = field.split(':').collect();
    if parts.len() != 4 {
        return Err(VaultError::Corrupt(format!(
            "SCRAM verifier has {} segments, expected 4",
            parts.len()
        )));
    }
    let salt =
        hex::decode(parts[0]).map_err(|_| VaultError::Corrupt("invalid SCRAM salt hex".into()))?;
    let iter: u32 = parts[1]
        .parse()
        .map_err(|_| VaultError::Corrupt("invalid SCRAM iter".into()))?;
    if iter < crate::auth::scram::MIN_ITER {
        return Err(VaultError::Corrupt(format!(
            "SCRAM iter {} below minimum {}",
            iter,
            crate::auth::scram::MIN_ITER
        )));
    }
    let stored_vec = hex::decode(parts[2])
        .map_err(|_| VaultError::Corrupt("invalid SCRAM stored_key hex".into()))?;
    let server_vec = hex::decode(parts[3])
        .map_err(|_| VaultError::Corrupt("invalid SCRAM server_key hex".into()))?;
    let stored_key: [u8; 32] = stored_vec
        .try_into()
        .map_err(|_| VaultError::Corrupt("SCRAM stored_key must be 32 bytes".into()))?;
    let server_key: [u8; 32] = server_vec
        .try_into()
        .map_err(|_| VaultError::Corrupt("SCRAM server_key must be 32 bytes".into()))?;
    Ok(crate::auth::scram::ScramVerifier {
        salt,
        iter,
        stored_key,
        server_key,
    })
}

/// Read the 16-byte salt from an existing vault page in the pager.
///
/// Works against both v1 (legacy) and v2 layouts because the salt sits at
/// the same offset (5..21) in both — we only need the salt to re-derive
/// the key, not to interpret the rest of the page. Callers that intend
/// to actually load() will hit the version check there if it's legacy.
fn read_vault_salt_from_pager(pager: &Pager) -> Result<[u8; 16], VaultError> {
    let page = pager
        .read_page_no_checksum(VAULT_HEADER_PAGE)
        .map_err(|e| VaultError::Pager(format!("vault page read: {e}")))?;

    let content = page.content();
    if content.len() < VAULT_HEADER_PREAMBLE_SIZE {
        return Err(VaultError::Corrupt("vault page too short".into()));
    }
    if &content[0..VAULT_MAGIC_SIZE] != VAULT_MAGIC {
        return Err(VaultError::Corrupt("bad magic bytes".into()));
    }

    let mut salt = [0u8; VAULT_SALT_SIZE];
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
                    tenant_id: None,
                    password_hash: "argon2id$aabbccdd$eeff0011".into(),
                    scram_verifier: None,
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
                    tenant_id: None,
                    password_hash: "argon2id$11223344$55667788".into(),
                    scram_verifier: None,
                    role: Role::Read,
                    api_keys: vec![],
                    created_at: now,
                    updated_at: now,
                    enabled: false,
                },
            ],
            api_keys: vec![(
                UserId::platform("alice"),
                ApiKey {
                    key: "rk_abc123".into(),
                    name: "ci-token".into(),
                    role: Role::Write,
                    created_at: now,
                },
            )],
            bootstrapped: true,
            master_secret: None,
            kv: std::collections::HashMap::new(),
        }
    }

    /// Helper to create a temporary pager for testing.
    fn temp_pager() -> (Pager, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_dir =
            std::env::temp_dir().join(format!("reddb_vault_test_{}_{}", std::process::id(), id));
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
        assert_eq!(restored.api_keys[0].0.username, "alice");
        assert!(restored.api_keys[0].0.tenant.is_none());
    }

    #[test]
    fn test_vault_state_empty() {
        let state = VaultState {
            users: vec![],
            api_keys: vec![],
            bootstrapped: false,
            master_secret: None,
            kv: std::collections::HashMap::new(),
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
            kv: std::collections::HashMap::new(),
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
            kv: std::collections::HashMap::new(),
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
            kv: std::collections::HashMap::new(),
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
            kv: std::collections::HashMap::new(),
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
            kv: std::collections::HashMap::new(),
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
    fn test_vault_state_scram_verifier_roundtrip() {
        use crate::auth::scram::ScramVerifier;

        let verifier = ScramVerifier::from_password(
            "hunter2",
            b"reddb-vault-test-salt".to_vec(),
            crate::auth::scram::DEFAULT_ITER,
        );

        let now = now_ms();
        let state = VaultState {
            users: vec![User {
                username: "carol".into(),
                tenant_id: None,
                password_hash: "argon2id$abc$def".into(),
                scram_verifier: Some(verifier.clone()),
                role: Role::Admin,
                api_keys: vec![],
                created_at: now,
                updated_at: now,
                enabled: true,
            }],
            api_keys: vec![],
            bootstrapped: true,
            master_secret: None,
            kv: std::collections::HashMap::new(),
        };

        let bytes = state.serialize();
        let restored = VaultState::deserialize(&bytes).unwrap();
        let carol = restored
            .users
            .iter()
            .find(|u| u.username == "carol")
            .unwrap();
        let v = carol.scram_verifier.as_ref().expect("verifier round-trips");
        assert_eq!(v.salt, verifier.salt);
        assert_eq!(v.iter, verifier.iter);
        assert_eq!(v.stored_key, verifier.stored_key);
        assert_eq!(v.server_key, verifier.server_key);
    }

    #[test]
    fn test_vault_state_pre_tenant_user_line_still_parses() {
        // 7-field pre-tenant USER line (no trailing tenant id). Must
        // keep working since vaults written before tenant scoping
        // landed have this shape.
        let now = now_ms();
        let line = format!(
            "USER:dave\targon2id$x$y\tread\ttrue\t{}\t{}\t\nSEALED:false\n",
            now, now
        );
        let restored = VaultState::deserialize(line.as_bytes()).unwrap();
        let dave = restored
            .users
            .iter()
            .find(|u| u.username == "dave")
            .unwrap();
        assert!(dave.scram_verifier.is_none());
        assert!(dave.tenant_id.is_none());
    }

    #[test]
    fn test_vault_state_user_line_with_tenant_roundtrip() {
        let now = now_ms();
        let state = VaultState {
            users: vec![User {
                username: "alice".into(),
                tenant_id: Some("acme".into()),
                password_hash: "argon2id$x$y".into(),
                scram_verifier: None,
                role: Role::Write,
                api_keys: vec![],
                created_at: now,
                updated_at: now,
                enabled: true,
            }],
            api_keys: vec![],
            bootstrapped: true,
            master_secret: None,
            kv: std::collections::HashMap::new(),
        };
        let bytes = state.serialize();
        let text = std::str::from_utf8(&bytes).unwrap();
        // 8 fields: trailing `\tacme` after the empty SCRAM field.
        assert!(text.contains("\tacme\n"));

        let restored = VaultState::deserialize(&bytes).unwrap();
        let alice = restored
            .users
            .iter()
            .find(|u| u.username == "alice")
            .unwrap();
        assert_eq!(alice.tenant_id.as_deref(), Some("acme"));
    }

    #[test]
    fn test_vault_state_key_line_with_tenant_reattaches_correctly() {
        // Two same-named users in different tenants. Each owns one
        // API key. Reattachment must respect tenant scope.
        let now = now_ms();
        let state = VaultState {
            users: vec![
                User {
                    username: "alice".into(),
                    tenant_id: Some("acme".into()),
                    password_hash: "argon2id$x$y".into(),
                    scram_verifier: None,
                    role: Role::Write,
                    api_keys: vec![],
                    created_at: now,
                    updated_at: now,
                    enabled: true,
                },
                User {
                    username: "alice".into(),
                    tenant_id: Some("globex".into()),
                    password_hash: "argon2id$a$b".into(),
                    scram_verifier: None,
                    role: Role::Read,
                    api_keys: vec![],
                    created_at: now,
                    updated_at: now,
                    enabled: true,
                },
            ],
            api_keys: vec![
                (
                    UserId::scoped("acme", "alice"),
                    ApiKey {
                        key: "rk_acme_key".into(),
                        name: "deploy".into(),
                        role: Role::Write,
                        created_at: now,
                    },
                ),
                (
                    UserId::scoped("globex", "alice"),
                    ApiKey {
                        key: "rk_globex_key".into(),
                        name: "ci".into(),
                        role: Role::Read,
                        created_at: now,
                    },
                ),
            ],
            bootstrapped: true,
            master_secret: None,
            kv: std::collections::HashMap::new(),
        };
        let bytes = state.serialize();
        let restored = VaultState::deserialize(&bytes).unwrap();
        // The api_keys vector retains both entries with the right
        // owners.
        assert_eq!(restored.api_keys.len(), 2);
        let acme_key = restored
            .api_keys
            .iter()
            .find(|(o, _)| o.tenant.as_deref() == Some("acme"))
            .unwrap();
        assert_eq!(acme_key.1.key, "rk_acme_key");
        let globex_key = restored
            .api_keys
            .iter()
            .find(|(o, _)| o.tenant.as_deref() == Some("globex"))
            .unwrap();
        assert_eq!(globex_key.1.key, "rk_globex_key");
    }

    #[test]
    fn test_vault_state_scram_iter_below_min_rejected() {
        let now = now_ms();
        // 33 hex pairs = 33 bytes, but the parse_scram_field iter check
        // fires before length validation. Stored/server are 32 hex bytes
        // (64 chars) here so we exercise the iter floor specifically.
        let stored_hex = "00".repeat(32);
        let server_hex = "11".repeat(32);
        let line = format!(
            "USER:eve\targon2id$x$y\tread\ttrue\t{}\t{}\tdeadbeef:1024:{}:{}\n",
            now, now, stored_hex, server_hex
        );
        match VaultState::deserialize(line.as_bytes()) {
            Err(VaultError::Corrupt(msg)) => assert!(msg.contains("below minimum")),
            Err(other) => panic!("expected Corrupt iter-floor error, got {other:?}"),
            Ok(_) => panic!("expected Corrupt iter-floor error, got Ok"),
        }
    }

    #[test]
    fn test_constant_time_eq_function() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"short", b"longer"));
        assert!(constant_time_eq(b"", b""));
    }
}
