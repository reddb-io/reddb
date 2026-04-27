//! Env-var-with-`_FILE`-companion helper (PLAN.md Phase 6.4).
//!
//! Centralises the pattern that ~5 modules independently reinvented:
//!   1. Read `VAR`. If non-empty/non-whitespace, return it.
//!   2. Otherwise read `VAR_FILE`. If set, treat its value as a path
//!      to a file containing the secret.
//!   3. `read_to_string` the file, trim trailing `\n`/`\r` (kubectl
//!      create secret + echo > file both append a newline), and
//!      return the contents if non-empty.
//!   4. On read failure, log a warning and return `None` — boot
//!      fails closed downstream when the secret was required.

/// Read `name` from the env. If unset/empty, fall back to
/// `<name>_FILE`. Returns `None` when neither produces a usable
/// value. Trims trailing whitespace from file contents.
pub fn env_with_file_fallback(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name) {
        if !value.trim().is_empty() {
            return Some(value);
        }
    }
    let file_var = format!("{name}_FILE");
    let path = std::env::var(&file_var).ok()?;
    let trimmed_path = path.trim();
    if trimmed_path.is_empty() {
        return None;
    }
    match std::fs::read_to_string(trimmed_path) {
        Ok(contents) => {
            let value = contents
                .trim_end_matches(|c: char| c == '\n' || c == '\r')
                .to_string();
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        }
        Err(err) => {
            tracing::warn!(
                target: "reddb::secrets",
                env = %file_var,
                path = %trimmed_path,
                error = %err,
                "secret file referenced by {file_var} could not be read; falling back to None"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The env namespace is process-global, so serialise the tests
    // that mutate it; otherwise running in parallel would create
    // false negatives.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn returns_inline_when_set() {
        let _g = env_lock().lock();
        unsafe {
            std::env::set_var("REDDB_TEST_INLINE", "value-from-env");
            std::env::remove_var("REDDB_TEST_INLINE_FILE");
        }
        assert_eq!(
            env_with_file_fallback("REDDB_TEST_INLINE"),
            Some("value-from-env".to_string())
        );
        unsafe {
            std::env::remove_var("REDDB_TEST_INLINE");
        }
    }

    #[test]
    fn falls_back_to_file_when_inline_empty() {
        let _g = env_lock().lock();
        let dir =
            std::env::temp_dir().join(format!("reddb-env-secret-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("token");
        std::fs::write(&path, "value-from-file\n").unwrap();
        unsafe {
            std::env::remove_var("REDDB_TEST_FALLBACK");
            std::env::set_var("REDDB_TEST_FALLBACK_FILE", &path);
        }
        assert_eq!(
            env_with_file_fallback("REDDB_TEST_FALLBACK"),
            Some("value-from-file".to_string()) // trailing \n trimmed
        );
        unsafe {
            std::env::remove_var("REDDB_TEST_FALLBACK_FILE");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_wins_over_file() {
        let _g = env_lock().lock();
        let dir = std::env::temp_dir().join(format!("reddb-env-precedence-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("token");
        std::fs::write(&path, "from-file").unwrap();
        unsafe {
            std::env::set_var("REDDB_TEST_PRIORITY", "from-env");
            std::env::set_var("REDDB_TEST_PRIORITY_FILE", &path);
        }
        assert_eq!(
            env_with_file_fallback("REDDB_TEST_PRIORITY"),
            Some("from-env".to_string())
        );
        unsafe {
            std::env::remove_var("REDDB_TEST_PRIORITY");
            std::env::remove_var("REDDB_TEST_PRIORITY_FILE");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn returns_none_when_neither_set() {
        let _g = env_lock().lock();
        unsafe {
            std::env::remove_var("REDDB_TEST_NONE");
            std::env::remove_var("REDDB_TEST_NONE_FILE");
        }
        assert_eq!(env_with_file_fallback("REDDB_TEST_NONE"), None);
    }

    #[test]
    fn read_failure_returns_none() {
        let _g = env_lock().lock();
        unsafe {
            std::env::remove_var("REDDB_TEST_BAD");
            std::env::set_var("REDDB_TEST_BAD_FILE", "/nonexistent/path/zzz");
        }
        assert_eq!(env_with_file_fallback("REDDB_TEST_BAD"), None);
        unsafe {
            std::env::remove_var("REDDB_TEST_BAD_FILE");
        }
    }
}
