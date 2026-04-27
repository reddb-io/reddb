//! `*_FILE` env-var expansion for containerised secret mounts.
//!
//! Industry-standard pattern from `postgres`, `mysql`, `redis` images:
//! operators mount a tmpfs secret at `/run/secrets/x` and set
//! `REDDB_PASSWORD_FILE=/run/secrets/x`. At boot we read the file,
//! place its contents in `REDDB_PASSWORD`, and strip the `_FILE` var
//! from the process env so it can't leak into `/proc/<pid>/environ`
//! or downstream `env` dumps.
//!
//! Differs from [`crate::utils::env_secret::env_with_file_fallback`]
//! by *writing into* the env (so downstream code that reads `VAR`
//! directly Just Works) and erroring on conflict instead of silently
//! preferring inline.
//!
//! Behaviour summary:
//!   * `<name>` set + `<name>_FILE` set    → error (operator mistake).
//!   * `<name>` set, `<name>_FILE` unset   → no-op.
//!   * `<name>_FILE` set                   → read file, trim trailing
//!     `\r?\n`, `set_var(<name>, contents)`, `remove_var(<name>_FILE)`.
//!   * file mode bits & 0o077 != 0         → `tracing::warn!` (still
//!     proceeds: world-readable secrets are an operator-fixable misconfig,
//!     not a hard fail).

/// Read the `_FILE` companion of `name` (if set), validate, place its
/// contents in `name`, and remove the `_FILE` var from the environment.
///
/// # Errors
/// * `InvalidInput` if both `name` and `<name>_FILE` are set.
/// * `NotFound` / `PermissionDenied` / etc. if the file cannot be read.
pub fn expand_file_env(name: &str) -> std::io::Result<()> {
    let file_var = format!("{name}_FILE");
    let path = match std::env::var(&file_var) {
        Ok(p) if !p.trim().is_empty() => p,
        // No `_FILE` companion → nothing to do.
        _ => return Ok(()),
    };

    // Conflict: refuse to silently pick a winner. The operator must
    // pick one form; supporting both invites split-brain incidents
    // (which one is current after a rotate?).
    if let Ok(existing) = std::env::var(name) {
        if !existing.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "both {name} and {file_var} are set — pick one (FILE form is for container/k8s secret mounts)"
                ),
            ));
        }
    }

    // World-readable mode is a misconfig, not a hard fail. Log and
    // proceed — flagging it loudly is more useful than refusing to
    // boot when /run/secrets/* on a single-tenant node happens to be
    // 0644 (common with hostPath mounts during dev).
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mode = meta.mode();
            if mode & 0o077 != 0 {
                tracing::warn!(
                    target: "reddb::secrets",
                    env = %file_var,
                    path = %path,
                    mode = format_args!("{:o}", mode & 0o7777),
                    "secret file is group/world-readable; consider chmod 0600"
                );
            }
        }
    }

    let contents = std::fs::read_to_string(&path)?;
    let value = contents
        .trim_end_matches(|c: char| c == '\n' || c == '\r')
        .to_string();

    // SAFETY: set_var/remove_var are unsafe in Rust 2024 because they
    // are not thread-safe relative to getenv. We invoke this only
    // from the very top of `main()` before any threads exist, so it
    // is sound.
    unsafe {
        std::env::set_var(name, &value);
        std::env::remove_var(&file_var);
    }

    tracing::info!(
        target: "reddb::secrets",
        env = %name,
        path = %path,
        "expanded {file_var} into {name}"
    );

    Ok(())
}

/// Apply [`expand_file_env`] to every standard RedDB secret env var.
///
/// Errors are returned as a Vec so we can surface every misconfig in
/// one boot rather than failing one-by-one across restarts. Caller
/// decides whether to abort.
pub fn expand_all_reddb_secrets() -> Vec<(String, std::io::Error)> {
    const VARS: &[&str] = &[
        "REDDB_CERTIFICATE",
        "REDDB_VAULT_KEY",
        "REDDB_USERNAME",
        "REDDB_PASSWORD",
        "REDDB_ROOT_TOKEN",
    ];
    let mut errors = Vec::new();
    for var in VARS {
        if let Err(err) = expand_file_env(var) {
            errors.push((var.to_string(), err));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The env namespace is process-global, so serialise tests that
    // mutate it; otherwise running in parallel would see each other's
    // writes and yield false negatives.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn tmpdir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "reddb-secret-file-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(name: &str) {
        unsafe {
            std::env::remove_var(name);
            std::env::remove_var(format!("{name}_FILE"));
        }
    }

    #[test]
    fn no_op_when_neither_set() {
        let _g = env_lock().lock();
        cleanup("REDDB_TEST_NOOP");
        assert!(expand_file_env("REDDB_TEST_NOOP").is_ok());
        assert!(std::env::var("REDDB_TEST_NOOP").is_err());
    }

    #[test]
    fn no_op_when_only_inline_set() {
        let _g = env_lock().lock();
        cleanup("REDDB_TEST_INLINE_ONLY");
        unsafe {
            std::env::set_var("REDDB_TEST_INLINE_ONLY", "inline-value");
        }
        assert!(expand_file_env("REDDB_TEST_INLINE_ONLY").is_ok());
        assert_eq!(
            std::env::var("REDDB_TEST_INLINE_ONLY").unwrap(),
            "inline-value"
        );
        cleanup("REDDB_TEST_INLINE_ONLY");
    }

    #[test]
    fn reads_file_and_strips_trailing_newline() {
        let _g = env_lock().lock();
        let dir = tmpdir("read");
        let path = dir.join("secret");
        std::fs::write(&path, "supersecret\n").unwrap();
        cleanup("REDDB_TEST_READ");
        unsafe {
            std::env::set_var("REDDB_TEST_READ_FILE", &path);
        }
        expand_file_env("REDDB_TEST_READ").unwrap();
        assert_eq!(std::env::var("REDDB_TEST_READ").unwrap(), "supersecret");
        // _FILE companion must be stripped after expansion.
        assert!(std::env::var("REDDB_TEST_READ_FILE").is_err());
        cleanup("REDDB_TEST_READ");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strips_crlf_endings() {
        let _g = env_lock().lock();
        let dir = tmpdir("crlf");
        let path = dir.join("secret");
        std::fs::write(&path, "windows-secret\r\n").unwrap();
        cleanup("REDDB_TEST_CRLF");
        unsafe {
            std::env::set_var("REDDB_TEST_CRLF_FILE", &path);
        }
        expand_file_env("REDDB_TEST_CRLF").unwrap();
        assert_eq!(std::env::var("REDDB_TEST_CRLF").unwrap(), "windows-secret");
        cleanup("REDDB_TEST_CRLF");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preserves_unicode() {
        let _g = env_lock().lock();
        let dir = tmpdir("unicode");
        let path = dir.join("secret");
        // Mix ASCII + multibyte UTF-8 to verify byte-trim doesn't slice mid-codepoint.
        std::fs::write(&path, "p4ss-✓-π\n").unwrap();
        cleanup("REDDB_TEST_UNI");
        unsafe {
            std::env::set_var("REDDB_TEST_UNI_FILE", &path);
        }
        expand_file_env("REDDB_TEST_UNI").unwrap();
        assert_eq!(std::env::var("REDDB_TEST_UNI").unwrap(), "p4ss-✓-π");
        cleanup("REDDB_TEST_UNI");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_when_both_inline_and_file_set() {
        let _g = env_lock().lock();
        let dir = tmpdir("conflict");
        let path = dir.join("secret");
        std::fs::write(&path, "from-file").unwrap();
        cleanup("REDDB_TEST_CONFLICT");
        unsafe {
            std::env::set_var("REDDB_TEST_CONFLICT", "from-env");
            std::env::set_var("REDDB_TEST_CONFLICT_FILE", &path);
        }
        let err = expand_file_env("REDDB_TEST_CONFLICT").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        // Existing inline must NOT be clobbered when the call errors.
        assert_eq!(std::env::var("REDDB_TEST_CONFLICT").unwrap(), "from-env");
        cleanup("REDDB_TEST_CONFLICT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_not_found() {
        let _g = env_lock().lock();
        cleanup("REDDB_TEST_MISSING");
        unsafe {
            std::env::set_var("REDDB_TEST_MISSING_FILE", "/nonexistent/zzz/reddb-no-file");
        }
        let err = expand_file_env("REDDB_TEST_MISSING").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        cleanup("REDDB_TEST_MISSING");
    }

    #[test]
    fn empty_file_path_var_is_no_op() {
        // Set _FILE to "" (whitespace-only path) — treated as unset to
        // match the env_with_file_fallback companion helper.
        let _g = env_lock().lock();
        cleanup("REDDB_TEST_EMPTYP");
        unsafe {
            std::env::set_var("REDDB_TEST_EMPTYP_FILE", "   ");
        }
        assert!(expand_file_env("REDDB_TEST_EMPTYP").is_ok());
        assert!(std::env::var("REDDB_TEST_EMPTYP").is_err());
        cleanup("REDDB_TEST_EMPTYP");
    }
}
