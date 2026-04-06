//! Keyring integration for secure password storage
//!
//! Stores the database encryption password in the system keyring
//! so users don't need to enter it every time.
//!
//! On Linux: Uses ~/.config/redblue/keyring (encrypted with user-specific key)
//! On macOS: Uses Keychain when available (TBD)
//! On Windows: Uses Credential Manager when available (TBD)

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::crypto::aes_gcm::{aes256_gcm_decrypt, aes256_gcm_encrypt};
use crate::crypto::sha256::sha256;
use crate::crypto::uuid::Uuid;

const SERVICE_NAME: &str = "redblue";
const KEYRING_FILE: &str = "keyring.enc";

/// Result of password resolution
#[derive(Debug, Clone)]
pub enum PasswordSource {
    /// Password from --db-password flag
    Flag(String),
    /// Password from REDBLUE_DB_KEY environment variable
    EnvVar(String),
    /// Password from system keyring
    Keyring(String),
    /// No password configured - database will be unencrypted
    None,
}

impl PasswordSource {
    pub fn password(&self) -> Option<&str> {
        match self {
            PasswordSource::Flag(p) => Some(p),
            PasswordSource::EnvVar(p) => Some(p),
            PasswordSource::Keyring(p) => Some(p),
            PasswordSource::None => None,
        }
    }

    pub fn is_encrypted(&self) -> bool {
        !matches!(self, PasswordSource::None)
    }

    pub fn source_name(&self) -> &'static str {
        match self {
            PasswordSource::Flag(_) => "flag",
            PasswordSource::EnvVar(_) => "env",
            PasswordSource::Keyring(_) => "keyring",
            PasswordSource::None => "none",
        }
    }
}

/// Resolve password from multiple sources (priority order)
/// 1. --db-password flag (highest priority)
/// 2. REDBLUE_DB_KEY environment variable
/// 3. System keyring
/// 4. None (no encryption)
pub fn resolve_password(flag_password: Option<&str>) -> PasswordSource {
    // Priority 1: Explicit flag
    if let Some(pwd) = flag_password {
        if !pwd.is_empty() {
            return PasswordSource::Flag(pwd.to_string());
        }
    }

    // Priority 2: Environment variable
    if let Ok(pwd) = std::env::var("REDBLUE_DB_KEY") {
        if !pwd.is_empty() {
            return PasswordSource::EnvVar(pwd);
        }
    }

    // Priority 3: System keyring
    if let Some(pwd) = get_from_keyring() {
        return PasswordSource::Keyring(pwd);
    }

    // Priority 4: No password
    PasswordSource::None
}

/// Get password from system keyring
pub fn get_from_keyring() -> Option<String> {
    let keyring_path = get_keyring_path()?;

    if !keyring_path.exists() {
        return None;
    }

    let mut file = fs::File::open(&keyring_path).ok()?;
    let mut encrypted_data = Vec::new();
    file.read_to_end(&mut encrypted_data).ok()?;

    if encrypted_data.len() < 28 {
        // Minimum: 12 (nonce) + 16 (tag)
        return None;
    }

    let key = derive_keyring_key();
    let nonce: [u8; 12] = encrypted_data[..12].try_into().ok()?;
    let ciphertext_and_tag = &encrypted_data[12..];

    let plaintext = aes256_gcm_decrypt(&key, &nonce, &[], ciphertext_and_tag).ok()?;

    String::from_utf8(plaintext).ok()
}

/// Save password to system keyring
pub fn save_to_keyring(password: &str) -> Result<(), String> {
    let keyring_path = get_keyring_path().ok_or("Failed to determine keyring path")?;

    // Ensure parent directory exists
    if let Some(parent) = keyring_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create keyring directory: {}", e))?;
    }

    let key = derive_keyring_key();

    // Generate random nonce
    let nonce = generate_nonce();

    // Encrypt password
    let ciphertext_and_tag = aes256_gcm_encrypt(&key, &nonce, &[], password.as_bytes());

    // Write: nonce || ciphertext || tag
    let mut data = Vec::with_capacity(12 + ciphertext_and_tag.len());
    data.extend_from_slice(&nonce);
    data.extend_from_slice(&ciphertext_and_tag);

    let mut file = fs::File::create(&keyring_path)
        .map_err(|e| format!("Failed to create keyring file: {}", e))?;
    file.write_all(&data)
        .map_err(|e| format!("Failed to write keyring: {}", e))?;

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        fs::set_permissions(&keyring_path, permissions)
            .map_err(|e| format!("Failed to set keyring permissions: {}", e))?;
    }

    Ok(())
}

/// Remove password from system keyring
pub fn clear_keyring() -> Result<(), String> {
    let keyring_path = get_keyring_path().ok_or("Failed to determine keyring path")?;

    if keyring_path.exists() {
        fs::remove_file(&keyring_path).map_err(|e| format!("Failed to remove keyring: {}", e))?;
    }

    Ok(())
}

/// Check if keyring has a stored password
pub fn has_keyring_password() -> bool {
    get_from_keyring().is_some()
}

/// Get keyring file path
fn get_keyring_path() -> Option<PathBuf> {
    // Try XDG config directory first
    if let Ok(config_dir) = std::env::var("XDG_CONFIG_HOME") {
        return Some(
            PathBuf::from(config_dir)
                .join(SERVICE_NAME)
                .join(KEYRING_FILE),
        );
    }

    // Fall back to ~/.config
    if let Ok(home) = std::env::var("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join(SERVICE_NAME)
                .join(KEYRING_FILE),
        );
    }

    // Windows fallback
    if let Ok(appdata) = std::env::var("APPDATA") {
        return Some(PathBuf::from(appdata).join(SERVICE_NAME).join(KEYRING_FILE));
    }

    None
}

/// Derive a unique key for keyring encryption based on machine/user identity
fn derive_keyring_key() -> [u8; 32] {
    let mut identity = String::new();

    // Add hostname
    if let Ok(hostname) = std::env::var("HOSTNAME") {
        identity.push_str(&hostname);
    } else if let Ok(name) = std::env::var("COMPUTERNAME") {
        identity.push_str(&name);
    }
    identity.push(':');

    // Add username
    if let Ok(user) = std::env::var("USER") {
        identity.push_str(&user);
    } else if let Ok(user) = std::env::var("USERNAME") {
        identity.push_str(&user);
    }
    identity.push(':');

    // Add home directory as additional entropy
    if let Ok(home) = std::env::var("HOME") {
        identity.push_str(&home);
    } else if let Ok(home) = std::env::var("USERPROFILE") {
        identity.push_str(&home);
    }

    // Add a fixed salt for the keyring
    identity.push_str(":redblue-keyring-v1");

    sha256(identity.as_bytes())
}

/// Generate a random 12-byte nonce
fn generate_nonce() -> [u8; 12] {
    let uuid = Uuid::new_v4();
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&uuid.as_bytes()[0..12]);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize keyring tests (they modify shared state)
    static KEYRING_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_password_source_is_encrypted() {
        assert!(PasswordSource::Flag("test".to_string()).is_encrypted());
        assert!(PasswordSource::EnvVar("test".to_string()).is_encrypted());
        assert!(PasswordSource::Keyring("test".to_string()).is_encrypted());
        assert!(!PasswordSource::None.is_encrypted());
    }

    #[test]
    fn test_password_source_name() {
        assert_eq!(PasswordSource::Flag("".to_string()).source_name(), "flag");
        assert_eq!(PasswordSource::EnvVar("".to_string()).source_name(), "env");
        assert_eq!(
            PasswordSource::Keyring("".to_string()).source_name(),
            "keyring"
        );
        assert_eq!(PasswordSource::None.source_name(), "none");
    }

    #[test]
    fn test_password_source_password() {
        assert_eq!(
            PasswordSource::Flag("mypass".to_string()).password(),
            Some("mypass")
        );
        assert_eq!(
            PasswordSource::EnvVar("envpass".to_string()).password(),
            Some("envpass")
        );
        assert_eq!(
            PasswordSource::Keyring("ringpass".to_string()).password(),
            Some("ringpass")
        );
        assert_eq!(PasswordSource::None.password(), None);
    }

    #[test]
    fn test_derive_keyring_key_deterministic() {
        let key1 = derive_keyring_key();
        let key2 = derive_keyring_key();
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 32);
    }

    #[test]
    fn test_derive_keyring_key_length() {
        let key = derive_keyring_key();
        assert_eq!(key.len(), 32); // AES-256 key
    }

    #[test]
    fn test_generate_nonce_uniqueness() {
        let nonce1 = generate_nonce();
        let nonce2 = generate_nonce();
        assert_ne!(nonce1, nonce2);
        assert_eq!(nonce1.len(), 12);
        assert_eq!(nonce2.len(), 12);
    }

    #[test]
    fn test_resolve_password_flag_priority() {
        // Flag should have highest priority
        let result = resolve_password(Some("flag_password"));
        assert!(matches!(result, PasswordSource::Flag(_)));
        if let PasswordSource::Flag(pwd) = result {
            assert_eq!(pwd, "flag_password");
        }
    }

    #[test]
    fn test_resolve_password_empty_flag() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();
        std::env::remove_var("REDBLUE_DB_KEY");
        let _ = clear_keyring();

        // Empty flag should not be used
        let result = resolve_password(Some(""));
        assert!(!matches!(result, PasswordSource::Flag(_)));
    }

    #[test]
    fn test_resolve_password_env_var() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();
        let _ = clear_keyring();

        std::env::set_var("REDBLUE_DB_KEY", "env_test_password");
        let result = resolve_password(None);
        std::env::remove_var("REDBLUE_DB_KEY");

        assert!(matches!(result, PasswordSource::EnvVar(_)));
        if let PasswordSource::EnvVar(pwd) = result {
            assert_eq!(pwd, "env_test_password");
        }
    }

    #[test]
    fn test_resolve_password_flag_overrides_env() {
        std::env::set_var("REDBLUE_DB_KEY", "env_password");
        let result = resolve_password(Some("flag_password"));
        std::env::remove_var("REDBLUE_DB_KEY");

        assert!(matches!(result, PasswordSource::Flag(_)));
    }

    #[test]
    fn test_keyring_save_and_retrieve() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();

        // Clear any existing keyring
        let _ = clear_keyring();

        // Save password
        let result = save_to_keyring("test_keyring_password_12345");
        assert!(result.is_ok(), "Failed to save to keyring: {:?}", result);

        // Retrieve password
        let retrieved = get_from_keyring();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), "test_keyring_password_12345");

        // Clean up
        let _ = clear_keyring();
    }

    #[test]
    fn test_keyring_has_password() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();

        let _ = clear_keyring();
        assert!(!has_keyring_password());

        let _ = save_to_keyring("check_password");
        assert!(has_keyring_password());

        let _ = clear_keyring();
        assert!(!has_keyring_password());
    }

    #[test]
    fn test_clear_keyring_nonexistent() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();

        // Should not error if keyring doesn't exist
        let _ = clear_keyring();
        let result = clear_keyring();
        assert!(result.is_ok());
    }

    #[test]
    fn test_keyring_special_characters() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();
        let _ = clear_keyring();

        // Test password with special characters
        let special_password = "p@$$w0rd!#%&*()[]{}|;':\",./<>?`~";
        let result = save_to_keyring(special_password);
        assert!(result.is_ok());

        let retrieved = get_from_keyring();
        assert_eq!(retrieved, Some(special_password.to_string()));

        let _ = clear_keyring();
    }

    #[test]
    fn test_keyring_unicode_password() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();
        let _ = clear_keyring();

        // Test password with unicode characters
        let unicode_password = "пароль🔒密码パスワード";
        let result = save_to_keyring(unicode_password);
        assert!(result.is_ok());

        let retrieved = get_from_keyring();
        assert_eq!(retrieved, Some(unicode_password.to_string()));

        let _ = clear_keyring();
    }

    #[test]
    fn test_keyring_empty_password() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();
        let _ = clear_keyring();

        // Even empty password should work
        let result = save_to_keyring("");
        assert!(result.is_ok());

        let retrieved = get_from_keyring();
        assert_eq!(retrieved, Some("".to_string()));

        let _ = clear_keyring();
    }

    #[test]
    fn test_keyring_long_password() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();
        let _ = clear_keyring();

        // Test very long password
        let long_password = "x".repeat(10000);
        let result = save_to_keyring(&long_password);
        assert!(result.is_ok());

        let retrieved = get_from_keyring();
        assert_eq!(retrieved, Some(long_password));

        let _ = clear_keyring();
    }

    #[test]
    fn test_resolve_password_keyring_integration() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();
        std::env::remove_var("REDBLUE_DB_KEY");
        let _ = clear_keyring();

        // Save to keyring
        let _ = save_to_keyring("keyring_test_pwd");

        // Should resolve from keyring
        let result = resolve_password(None);
        assert!(matches!(result, PasswordSource::Keyring(_)));
        if let PasswordSource::Keyring(pwd) = result {
            assert_eq!(pwd, "keyring_test_pwd");
        }

        let _ = clear_keyring();
    }

    #[test]
    fn test_resolve_password_none_when_empty() {
        let _lock = KEYRING_TEST_LOCK.lock().unwrap();
        std::env::remove_var("REDBLUE_DB_KEY");
        let _ = clear_keyring();

        let result = resolve_password(None);
        assert!(matches!(result, PasswordSource::None));
    }

    #[test]
    fn test_get_keyring_path_returns_some() {
        // Should return a path on most systems
        let path = get_keyring_path();
        // This might be None in very restricted environments
        if path.is_some() {
            let p = path.unwrap();
            assert!(p.to_string_lossy().contains("redblue"));
            assert!(p.to_string_lossy().contains("keyring.enc"));
        }
    }
}
