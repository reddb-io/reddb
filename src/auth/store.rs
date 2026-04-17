//! AuthStore -- manages users, sessions, and API keys in memory.
//!
//! Password hashing delegates to the existing Argon2id implementation in
//! `crate::storage::encryption::argon2id`.  Token generation uses the
//! OS CSPRNG (`crate::crypto::os_random`) plus SHA-256.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::crypto::os_random;
use crate::crypto::sha256::sha256;
use crate::storage::encryption::argon2id::{derive_key, Argon2Params};
use crate::storage::engine::pager::Pager;

use super::vault::{KeyPair, Vault, VaultState};
use super::{now_ms, ApiKey, AuthConfig, AuthError, Role, Session, User};

// ---------------------------------------------------------------------------
// BootstrapResult
// ---------------------------------------------------------------------------

/// Result of a successful bootstrap operation.
///
/// The `certificate` is the hex-encoded string the admin must save --
/// it is the ONLY way to unseal the vault after a restart.
#[derive(Debug)]
pub struct BootstrapResult {
    pub user: User,
    pub api_key: ApiKey,
    /// Certificate hex string.  `None` when vault is not configured.
    pub certificate: Option<String>,
}

// ---------------------------------------------------------------------------
// AuthStore
// ---------------------------------------------------------------------------

/// Central in-process authority for auth state.
///
/// All mutations are guarded by `RwLock`s so the store is `Send + Sync`.
pub struct AuthStore {
    users: RwLock<HashMap<String, User>>,
    sessions: RwLock<HashMap<String, Session>>,
    /// key-string -> (username, role)
    api_key_index: RwLock<HashMap<String, (String, Role)>>,
    /// Once true, bootstrap() is permanently sealed.
    bootstrapped: AtomicBool,
    config: AuthConfig,
    /// Optional encrypted vault for persisting auth state to pager pages.
    vault: RwLock<Option<Vault>>,
    /// Reference to the pager for vault page I/O.
    pager: Option<Arc<Pager>>,
    /// Certificate-based keypair for token signing and vault seal.
    /// Populated after bootstrap or after restoring from a sealed vault.
    keypair: RwLock<Option<KeyPair>>,
    /// Encrypted key-value store for arbitrary secrets.
    /// Persisted to vault alongside users/api_keys.
    vault_kv: RwLock<HashMap<String, String>>,
}

// Use fast-but-safe Argon2id params for auth hashing (smaller than the
// default 64 MB so that user-management RPCs respond quickly).
fn auth_argon2_params() -> Argon2Params {
    Argon2Params {
        m_cost: 4 * 1024, // 4 MB
        t_cost: 3,
        p: 1,
        tag_len: 32,
    }
}

impl AuthStore {
    // -----------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------

    pub fn new(config: AuthConfig) -> Self {
        Self {
            users: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            api_key_index: RwLock::new(HashMap::new()),
            bootstrapped: AtomicBool::new(false),
            config,
            vault: RwLock::new(None),
            pager: None,
            keypair: RwLock::new(None),
            vault_kv: RwLock::new(HashMap::new()),
        }
    }

    /// Create an `AuthStore` backed by encrypted vault pages inside the
    /// main `.rdb` database file.
    ///
    /// If vault pages already exist, their contents are loaded and
    /// restored into the in-memory store.  All subsequent mutations are
    /// automatically persisted to the vault pages via the pager.
    pub fn with_vault(
        config: AuthConfig,
        pager: Arc<Pager>,
        passphrase: Option<&str>,
    ) -> Result<Self, AuthError> {
        let vault = Vault::open(&pager, passphrase)?;
        let mut store = Self::new(config);

        // Restore persisted state if vault pages exist.
        if let Some(state) = vault.load(&pager)? {
            store.restore_from_vault(state);
        }

        *store.vault.write().unwrap_or_else(|e| e.into_inner()) = Some(vault);
        store.pager = Some(pager);
        Ok(store)
    }

    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Returns true when no users exist yet and bootstrap hasn't been sealed.
    pub fn needs_bootstrap(&self) -> bool {
        !self.bootstrapped.load(Ordering::Acquire)
            && self.users.read().map(|u| u.is_empty()).unwrap_or(true)
    }

    /// Whether bootstrap has already been performed (sealed).
    pub fn is_bootstrapped(&self) -> bool {
        self.bootstrapped.load(Ordering::Acquire)
    }

    /// Bootstrap the first admin user. One-shot, irreversible.
    ///
    /// Uses an atomic compare-exchange to guarantee that even under
    /// concurrent calls, only the first one succeeds. Once sealed,
    /// all subsequent calls fail immediately -- there is no undo.
    ///
    /// When a vault/pager is configured, a certificate-based keypair is
    /// generated and the vault is re-encrypted with the certificate-derived
    /// key.  The certificate hex string is returned in `BootstrapResult`
    /// so the admin can save it.
    pub fn bootstrap(&self, username: &str, password: &str) -> Result<BootstrapResult, AuthError> {
        // Atomic seal: only the first caller wins.
        if self
            .bootstrapped
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(AuthError::Forbidden(
                "bootstrap already completed — sealed permanently".to_string(),
            ));
        }

        // Double-check users are actually empty (belt and suspenders).
        {
            let users = self.users.read().map_err(lock_err)?;
            if !users.is_empty() {
                return Err(AuthError::Forbidden(
                    "bootstrap already completed — users exist".to_string(),
                ));
            }
        }

        let user = self.create_user(username, password, Role::Admin)?;
        let key = self.create_api_key(username, "bootstrap", Role::Admin)?;

        // Generate a certificate-based keypair and re-seal the vault.
        let certificate = if let Some(ref pager) = self.pager {
            let kp = KeyPair::generate();
            let cert_hex = kp.certificate_hex();

            // Re-create the vault using the certificate-derived key.
            let new_vault = Vault::with_certificate_bytes(pager, &kp.certificate)
                .map_err(|e| AuthError::Internal(format!("vault re-seal failed: {e}")))?;

            // Store the keypair so token signing works immediately.
            if let Ok(mut kp_guard) = self.keypair.write() {
                *kp_guard = Some(kp);
            }

            // Replace the vault and persist with the master secret included.
            if let Ok(mut vault_guard) = self.vault.write() {
                *vault_guard = Some(new_vault);
            }
            // Generate the AES-256 secret key for Value::Secret encryption.
            self.ensure_vault_secret_key();
            self.persist_to_vault();

            Some(cert_hex)
        } else {
            None
        };

        Ok(BootstrapResult {
            user,
            api_key: key,
            certificate,
        })
    }

    /// Auto-bootstrap from environment variables if no users exist.
    ///
    /// Checks `REDDB_USERNAME` and `REDDB_PASSWORD`. If both are set and
    /// the user store is empty, creates the first admin user automatically.
    /// This mirrors the Docker pattern (`MYSQL_ROOT_PASSWORD`, etc.).
    ///
    /// Returns `Some(BootstrapResult)` if bootstrapped, `None` if skipped.
    pub fn bootstrap_from_env(&self) -> Option<BootstrapResult> {
        if !self.needs_bootstrap() {
            return None;
        }

        let username = std::env::var("REDDB_USERNAME").ok()?;
        let password = std::env::var("REDDB_PASSWORD").ok()?;

        if username.is_empty() || password.is_empty() {
            return None;
        }

        match self.bootstrap(&username, &password) {
            Ok(result) => {
                tracing::info!(
                    username = %username,
                    "bootstrapped admin user from REDDB_USERNAME/REDDB_PASSWORD"
                );
                if let Some(ref cert) = result.certificate {
                    // Certificate must be readable by the operator — keep it
                    // in the log stream but print raw to stderr too so it
                    // survives even if the log file gets rotated.
                    eprintln!("[reddb] CERTIFICATE: {}", cert);
                    tracing::warn!(
                        "vault certificate issued — save it: ONLY way to unseal after restart"
                    );
                }
                Some(result)
            }
            Err(e) => {
                tracing::error!(err = %e, "env bootstrap failed");
                None
            }
        }
    }

    // -----------------------------------------------------------------
    // Vault persistence
    // -----------------------------------------------------------------

    /// Persist the current auth state to the vault pages (if configured).
    ///
    /// Called automatically after every mutation.  Errors are logged but
    /// do not propagate -- the in-memory state is always authoritative.
    fn persist_to_vault(&self) {
        let vault_guard = self.vault.read().unwrap_or_else(|e| e.into_inner());
        if let (Some(ref vault), Some(ref pager)) = (&*vault_guard, &self.pager) {
            let state = self.snapshot();
            if let Err(e) = vault.save(pager, &state) {
                tracing::error!(err = %e, "vault persist failed");
            }
        }
    }

    // -----------------------------------------------------------------
    // Vault KV — encrypted key-value store for arbitrary secrets
    // -----------------------------------------------------------------

    /// Read a value from the vault KV store. Returns `None` if not set.
    pub fn vault_kv_get(&self, key: &str) -> Option<String> {
        self.vault_kv
            .read()
            .ok()
            .and_then(|kv| kv.get(key).cloned())
    }

    /// Write a value to the vault KV store, persisting to disk.
    pub fn vault_kv_set(&self, key: String, value: String) {
        if let Ok(mut kv) = self.vault_kv.write() {
            kv.insert(key, value);
        }
        self.persist_to_vault();
    }

    /// Delete a value from the vault KV store. Returns true if it existed.
    pub fn vault_kv_delete(&self, key: &str) -> bool {
        let existed = self
            .vault_kv
            .write()
            .map(|mut kv| kv.remove(key).is_some())
            .unwrap_or(false);
        if existed {
            self.persist_to_vault();
        }
        existed
    }

    /// List all keys in the vault KV store.
    pub fn vault_kv_keys(&self) -> Vec<String> {
        self.vault_kv
            .read()
            .map(|kv| kv.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Convenience: get the 32-byte secret key for Value::Secret encryption.
    /// Generated on first boot and stored at `red.secret.aes_key`.
    pub fn vault_secret_key(&self) -> Option<[u8; 32]> {
        let hex_str = self.vault_kv_get("red.secret.aes_key")?;
        let bytes = hex::decode(hex_str).ok()?;
        if bytes.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            Some(key)
        } else {
            None
        }
    }

    /// Generate and store the AES-256 secret key on first boot if not present.
    pub fn ensure_vault_secret_key(&self) {
        if self.vault_kv_get("red.secret.aes_key").is_none() {
            let key = random_bytes(32);
            self.vault_kv_set("red.secret.aes_key".to_string(), hex::encode(key));
        }
    }

    /// Take a snapshot of the current auth state for vault serialization.
    fn snapshot(&self) -> VaultState {
        let users_guard = self.users.read().unwrap_or_else(|e| e.into_inner());
        let users: Vec<User> = users_guard.values().cloned().collect();

        // Collect (owner_username, api_key) pairs from all users.
        let mut api_keys = Vec::new();
        for user in &users {
            for key in &user.api_keys {
                api_keys.push((user.username.clone(), key.clone()));
            }
        }

        // Include the master secret if a keypair is loaded.
        let master_secret = self
            .keypair
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|kp| kp.master_secret.clone()));

        let kv = self.vault_kv.read().map(|m| m.clone()).unwrap_or_default();

        VaultState {
            users,
            api_keys,
            bootstrapped: self.bootstrapped.load(Ordering::Acquire),
            master_secret,
            kv,
        }
    }

    /// Restore the in-memory auth state from a vault snapshot.
    fn restore_from_vault(&mut self, state: VaultState) {
        // Restore bootstrap seal.
        if state.bootstrapped {
            self.bootstrapped.store(true, Ordering::Release);
        }

        // Restore keypair from master secret (if present).
        if let Some(secret) = state.master_secret {
            let kp = KeyPair::from_master_secret(secret);
            if let Ok(mut guard) = self.keypair.write() {
                *guard = Some(kp);
            }
        }

        // Restore KV store.
        if let Ok(mut kv) = self.vault_kv.write() {
            *kv = state.kv;
        }

        // Restore users.
        let mut users = self.users.write().unwrap_or_else(|e| e.into_inner());
        let mut idx = self
            .api_key_index
            .write()
            .unwrap_or_else(|e| e.into_inner());

        for user in state.users {
            // Register API keys in the index.
            for key in &user.api_keys {
                idx.insert(key.key.clone(), (user.username.clone(), key.role));
            }
            users.insert(user.username.clone(), user);
        }
    }

    // -----------------------------------------------------------------
    // User management
    // -----------------------------------------------------------------

    /// Create a new user with the given role.
    pub fn create_user(
        &self,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<User, AuthError> {
        let mut users = self.users.write().map_err(lock_err)?;
        if users.contains_key(username) {
            return Err(AuthError::UserExists(username.to_string()));
        }

        let now = now_ms();
        let user = User {
            username: username.to_string(),
            password_hash: hash_password(password),
            role,
            api_keys: Vec::new(),
            created_at: now,
            updated_at: now,
            enabled: true,
        };
        users.insert(username.to_string(), user.clone());
        drop(users); // release lock before vault I/O
        self.persist_to_vault();
        Ok(user)
    }

    /// Return all users (password hashes redacted).
    pub fn list_users(&self) -> Vec<User> {
        let users = match self.users.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        users
            .values()
            .map(|u| User {
                password_hash: String::new(), // redacted
                ..u.clone()
            })
            .collect()
    }

    /// Delete a user and revoke all of their API keys + sessions.
    pub fn delete_user(&self, username: &str) -> Result<(), AuthError> {
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .remove(username)
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

        // Remove API key index entries.
        if let Ok(mut idx) = self.api_key_index.write() {
            for api_key in &user.api_keys {
                idx.remove(&api_key.key);
            }
        }

        // Remove sessions belonging to this user.
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.retain(|_, s| s.username != username);
        }

        self.persist_to_vault();
        Ok(())
    }

    /// Change password (requires the old password).
    pub fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), AuthError> {
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .get_mut(username)
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

        if !verify_password(old_password, &user.password_hash) {
            return Err(AuthError::InvalidCredentials);
        }

        user.password_hash = hash_password(new_password);
        user.updated_at = now_ms();
        drop(users); // release lock before vault I/O
        self.persist_to_vault();
        Ok(())
    }

    /// Change a user's role (admin-only operation).
    pub fn change_role(&self, username: &str, new_role: Role) -> Result<(), AuthError> {
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .get_mut(username)
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

        user.role = new_role;
        user.updated_at = now_ms();

        // Downgrade any API keys that now exceed the user's role.
        for key in &mut user.api_keys {
            if key.role > new_role {
                key.role = new_role;
            }
        }

        // Update the api_key_index as well.
        if let Ok(mut idx) = self.api_key_index.write() {
            for key in &user.api_keys {
                if let Some(entry) = idx.get_mut(&key.key) {
                    entry.1 = key.role;
                }
            }
        }

        self.persist_to_vault();
        Ok(())
    }

    // -----------------------------------------------------------------
    // Authentication (login)
    // -----------------------------------------------------------------

    /// Verify credentials and create a session.
    ///
    /// When a keypair is available (certificate-based seal), session tokens
    /// are signed with the master secret so the server can verify they were
    /// genuinely issued by this vault instance.
    pub fn authenticate(&self, username: &str, password: &str) -> Result<Session, AuthError> {
        let users = self.users.read().map_err(lock_err)?;
        let user = users.get(username).ok_or(AuthError::InvalidCredentials)?;

        if !user.enabled {
            return Err(AuthError::InvalidCredentials);
        }

        if !verify_password(password, &user.password_hash) {
            return Err(AuthError::InvalidCredentials);
        }

        // Generate token: signed if keypair is available, random otherwise.
        let token = match self.keypair.read().ok().and_then(|g| {
            g.as_ref().map(|kp| {
                let token_id = random_hex(16);
                let sig = kp.sign(format!("session:{}", token_id).as_bytes());
                // Take first 16 bytes of signature for compact token.
                format!("rs_{}{}", token_id, hex::encode(&sig[..16]))
            })
        }) {
            Some(signed_token) => signed_token,
            None => generate_session_token(),
        };

        let now = now_ms();
        let session = Session {
            token,
            username: username.to_string(),
            role: user.role,
            created_at: now,
            expires_at: now + (self.config.session_ttl_secs as u128 * 1000),
        };

        drop(users); // release read lock before acquiring write

        let mut sessions = self.sessions.write().map_err(lock_err)?;
        sessions.insert(session.token.clone(), session.clone());
        Ok(session)
    }

    // -----------------------------------------------------------------
    // Token validation
    // -----------------------------------------------------------------

    /// Validate a token (session or API key).
    ///
    /// Returns `(username, role)` if valid, `None` otherwise.
    pub fn validate_token(&self, token: &str) -> Option<(String, Role)> {
        // Try session tokens first.
        if token.starts_with("rs_") {
            if let Ok(sessions) = self.sessions.read() {
                if let Some(session) = sessions.get(token) {
                    let now = now_ms();
                    if now < session.expires_at {
                        return Some((session.username.clone(), session.role));
                    }
                }
            }
            return None;
        }

        // Try API keys.
        if token.starts_with("rk_") {
            if let Ok(idx) = self.api_key_index.read() {
                return idx.get(token).cloned();
            }
            return None;
        }

        None
    }

    // -----------------------------------------------------------------
    // API Key management
    // -----------------------------------------------------------------

    /// Create a persistent API key for a user.
    pub fn create_api_key(
        &self,
        username: &str,
        name: &str,
        role: Role,
    ) -> Result<ApiKey, AuthError> {
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .get_mut(username)
            .ok_or_else(|| AuthError::UserNotFound(username.to_string()))?;

        // The key's role cannot exceed the user's role.
        if role > user.role {
            return Err(AuthError::RoleExceeded {
                requested: role,
                ceiling: user.role,
            });
        }

        let api_key = ApiKey {
            key: generate_api_key(),
            name: name.to_string(),
            role,
            created_at: now_ms(),
        };

        user.api_keys.push(api_key.clone());
        user.updated_at = now_ms();

        // Update the index.
        if let Ok(mut idx) = self.api_key_index.write() {
            idx.insert(api_key.key.clone(), (username.to_string(), api_key.role));
        }

        drop(users); // release lock before vault I/O
        self.persist_to_vault();
        Ok(api_key)
    }

    /// Revoke (delete) an API key.
    pub fn revoke_api_key(&self, key: &str) -> Result<(), AuthError> {
        let mut users = self.users.write().map_err(lock_err)?;

        // Find which user owns this key.
        let owner = users
            .values()
            .find(|u| u.api_keys.iter().any(|k| k.key == key));
        let owner_name = match owner {
            Some(u) => u.username.clone(),
            None => return Err(AuthError::KeyNotFound(key.to_string())),
        };

        let user = users
            .get_mut(&owner_name)
            .ok_or_else(|| AuthError::KeyNotFound(key.to_string()))?;
        user.api_keys.retain(|k| k.key != key);
        user.updated_at = now_ms();

        // Remove from index.
        if let Ok(mut idx) = self.api_key_index.write() {
            idx.remove(key);
        }

        self.persist_to_vault();
        Ok(())
    }

    // -----------------------------------------------------------------
    // Session management
    // -----------------------------------------------------------------

    /// Revoke a session token.
    pub fn revoke_session(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(token);
        }
    }

    /// Purge expired sessions (housekeeping).
    pub fn purge_expired_sessions(&self) -> usize {
        let now = now_ms();
        if let Ok(mut sessions) = self.sessions.write() {
            let before = sessions.len();
            sessions.retain(|_, s| s.expires_at > now);
            return before - sessions.len();
        }
        0
    }
}

// ===========================================================================
// Password hashing
// ===========================================================================

/// Hash a password using Argon2id.
///
/// Format: `argon2id$<salt_hex>$<hash_hex>`
pub(crate) fn hash_password(password: &str) -> String {
    let salt = random_bytes(16);
    let params = auth_argon2_params();
    let hash = derive_key(password.as_bytes(), &salt, &params);
    format!("argon2id${}${}", hex::encode(&salt), hex::encode(&hash))
}

/// Verify a password against a stored `argon2id$<salt>$<hash>` string.
pub(crate) fn verify_password(password: &str, stored_hash: &str) -> bool {
    let parts: Vec<&str> = stored_hash.splitn(3, '$').collect();
    if parts.len() != 3 || parts[0] != "argon2id" {
        return false;
    }

    let salt = match hex::decode(parts[1]) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let expected_hash = match hex::decode(parts[2]) {
        Ok(h) => h,
        Err(_) => return false,
    };

    let params = auth_argon2_params();
    let computed = derive_key(password.as_bytes(), &salt, &params);
    constant_time_eq(&computed, &expected_hash)
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

// ===========================================================================
// Token generation
// ===========================================================================

fn generate_session_token() -> String {
    format!("rs_{}", hex::encode(random_bytes(32)))
}

fn generate_api_key() -> String {
    format!("rk_{}", hex::encode(random_bytes(32)))
}

/// Generate `n` random bytes and return as a hex string.
fn random_hex(n: usize) -> String {
    hex::encode(random_bytes(n))
}

/// Generate `n` cryptographically random bytes using the OS CSPRNG,
/// then mix with SHA-256 for domain separation.
pub(crate) fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n.max(32)];
    if os_random::fill_bytes(&mut buf).is_err() {
        // Fallback: use system time and pointers as entropy (best-effort).
        let seed = now_ms().to_le_bytes();
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = seed[i % seed.len()] ^ (i as u8);
        }
    }
    // SHA-256 mix to ensure uniform distribution.
    let digest = sha256(&buf);
    if n <= 32 {
        digest[..n].to_vec()
    } else {
        // Chain SHA-256 for longer outputs (unusual but supported).
        let mut out = Vec::with_capacity(n);
        let mut prev = digest;
        while out.len() < n {
            out.extend_from_slice(&prev[..std::cmp::min(32, n - out.len())]);
            prev = sha256(&prev);
        }
        out
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn lock_err<T>(_: T) -> AuthError {
    AuthError::Internal("lock poisoned".to_string())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AuthConfig {
        AuthConfig {
            enabled: true,
            session_ttl_secs: 60,
            require_auth: true,
            auto_encrypt_storage: false,
            vault_enabled: false,
        }
    }

    #[test]
    fn test_create_and_list_users() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass1", Role::Admin).unwrap();
        store.create_user("bob", "pass2", Role::Read).unwrap();

        let users = store.list_users();
        assert_eq!(users.len(), 2);
        // Password hashes should be redacted.
        for u in &users {
            assert!(u.password_hash.is_empty());
        }
    }

    #[test]
    fn test_create_duplicate_user() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Admin).unwrap();
        let err = store.create_user("alice", "pass2", Role::Read).unwrap_err();
        assert!(matches!(err, AuthError::UserExists(_)));
    }

    #[test]
    fn test_authenticate_and_validate() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "secret", Role::Write).unwrap();

        let session = store.authenticate("alice", "secret").unwrap();
        assert!(session.token.starts_with("rs_"));

        let (username, role) = store.validate_token(&session.token).unwrap();
        assert_eq!(username, "alice");
        assert_eq!(role, Role::Write);
    }

    #[test]
    fn test_authenticate_wrong_password() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "secret", Role::Read).unwrap();

        let err = store.authenticate("alice", "wrong").unwrap_err();
        assert!(matches!(err, AuthError::InvalidCredentials));
    }

    #[test]
    fn test_api_key_lifecycle() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Admin).unwrap();

        let key = store
            .create_api_key("alice", "ci-token", Role::Write)
            .unwrap();
        assert!(key.key.starts_with("rk_"));

        let (username, role) = store.validate_token(&key.key).unwrap();
        assert_eq!(username, "alice");
        assert_eq!(role, Role::Write);

        store.revoke_api_key(&key.key).unwrap();
        assert!(store.validate_token(&key.key).is_none());
    }

    #[test]
    fn test_api_key_role_exceeded() {
        let store = AuthStore::new(test_config());
        store.create_user("bob", "pass", Role::Read).unwrap();

        let err = store
            .create_api_key("bob", "escalate", Role::Admin)
            .unwrap_err();
        assert!(matches!(err, AuthError::RoleExceeded { .. }));
    }

    #[test]
    fn test_change_password() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "old", Role::Write).unwrap();

        store.change_password("alice", "old", "new").unwrap();

        // Old password should fail.
        assert!(store.authenticate("alice", "old").is_err());
        // New password should succeed.
        assert!(store.authenticate("alice", "new").is_ok());
    }

    #[test]
    fn test_change_role() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Admin).unwrap();
        store.create_api_key("alice", "key1", Role::Admin).unwrap();

        store.change_role("alice", Role::Read).unwrap();

        // User's role should be Read now.
        let users = store.list_users();
        let alice = users.iter().find(|u| u.username == "alice").unwrap();
        assert_eq!(alice.role, Role::Read);

        // API keys should have been downgraded.
        assert_eq!(alice.api_keys[0].role, Role::Read);
    }

    #[test]
    fn test_delete_user() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Admin).unwrap();
        let key = store.create_api_key("alice", "key1", Role::Read).unwrap();
        let session = store.authenticate("alice", "pass").unwrap();

        store.delete_user("alice").unwrap();

        assert!(store.validate_token(&key.key).is_none());
        assert!(store.validate_token(&session.token).is_none());
        assert!(store.list_users().is_empty());
    }

    #[test]
    fn test_revoke_session() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Read).unwrap();
        let session = store.authenticate("alice", "pass").unwrap();

        store.revoke_session(&session.token);
        assert!(store.validate_token(&session.token).is_none());
    }

    #[test]
    fn test_password_hash_format() {
        let hash = hash_password("test");
        assert!(hash.starts_with("argon2id$"));
        let parts: Vec<&str> = hash.splitn(3, '$').collect();
        assert_eq!(parts.len(), 3);
        // Salt is 16 bytes = 32 hex chars.
        assert_eq!(parts[1].len(), 32);
        // Hash is 32 bytes = 64 hex chars.
        assert_eq!(parts[2].len(), 64);
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn test_bootstrap_seals_permanently() {
        let store = AuthStore::new(test_config());

        assert!(store.needs_bootstrap());
        assert!(!store.is_bootstrapped());

        // First bootstrap succeeds
        let result = store.bootstrap("admin", "secret");
        assert!(result.is_ok());
        let br = result.unwrap();
        assert_eq!(br.user.username, "admin");
        assert_eq!(br.user.role, Role::Admin);
        assert!(br.api_key.key.starts_with("rk_"));
        // No vault configured, so no certificate.
        assert!(br.certificate.is_none());

        // Sealed now
        assert!(!store.needs_bootstrap());
        assert!(store.is_bootstrapped());

        // Second bootstrap fails -- sealed permanently
        let result = store.bootstrap("admin2", "secret2");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("sealed permanently"));

        // Only 1 user exists (the first one)
        assert_eq!(store.list_users().len(), 1);
        assert_eq!(store.list_users()[0].username, "admin");
    }

    #[test]
    fn test_bootstrap_after_manual_user_creation() {
        let store = AuthStore::new(test_config());

        // Create a user manually first
        store.create_user("existing", "pass", Role::Read).unwrap();

        // Bootstrap sees the seal hasn't been set but users exist
        // The atomic seal fires first, then the users check catches it
        assert!(!store.needs_bootstrap()); // users exist → false
    }
}
