//! `red bootstrap` — headless first-admin bootstrap for containers.
//!
//! Designed for K8s Jobs / CI pipelines that mount a tmpfs secret with
//! the admin password and need a one-shot binary that:
//!   1. Opens (or creates) the database file at `--path`.
//!   2. Opens the encrypted vault (requires `REDDB_CERTIFICATE` or
//!      `REDDB_VAULT_KEY` — typically via the `_FILE` companion).
//!   3. Calls `AuthStore::bootstrap` once.
//!   4. Prints the freshly-issued certificate so the operator can
//!      capture it (it is the ONLY way to unseal the vault later).
//!
//! Exits non-zero on any failure (already bootstrapped, missing vault
//! key, file open error, ...).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::auth::store::AuthStore;
use crate::auth::AuthConfig;
use crate::{RedDBOptions, RedDBRuntime};

/// Parsed args for `red bootstrap`. Constructed by the bin dispatcher
/// from the CLI flag map; kept as a plain struct so the unit tests
/// don't have to drag in a tokenizer.
pub struct BootstrapArgs {
    pub path: PathBuf,
    pub vault: bool,
    pub username: String,
    /// Provided by `--password`. None when `--password-stdin` will
    /// supply it.
    pub password: Option<String>,
    pub password_stdin: bool,
    pub print_certificate: bool,
    pub json: bool,
}

/// Outcome rendered by [`run`] on success.
#[derive(Debug)]
pub struct BootstrapOutcome {
    pub username: String,
    pub api_key: String,
    pub certificate: String,
}

/// Execute the bootstrap subcommand. Caller is responsible for
/// process exit; we return Result so the dispatcher can format errors
/// in the requested envelope (text vs JSON).
pub fn run(args: BootstrapArgs) -> Result<BootstrapOutcome, String> {
    if !args.vault {
        // Vault is mandatory: bootstrapping without one would issue an
        // admin password that lives only in unencrypted pages. That is
        // never what the operator wants for a credentialled cluster.
        return Err(
            "bootstrap requires --vault (admin credentials must be sealed in the encrypted vault)"
                .to_string(),
        );
    }

    if std::env::var("REDDB_CERTIFICATE")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
        && std::env::var("REDDB_VAULT_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .is_none()
    {
        return Err("vault requires REDDB_CERTIFICATE or REDDB_VAULT_KEY (use the *_FILE companion to read from a mounted secret)".to_string());
    }

    if args.username.trim().is_empty() {
        return Err(
            "username is required (use --username, or set REDDB_USERNAME / REDDB_USERNAME_FILE)"
                .to_string(),
        );
    }

    let password = resolve_password(&args)?;
    if password.is_empty() {
        return Err("password is required (use --password-stdin or REDDB_PASSWORD_FILE)".into());
    }

    // Open the runtime in persistent mode at the requested path. The
    // engine creates the file on first open, so this works for both
    // green-field bootstraps and re-runs against an existing DB.
    let opts = RedDBOptions::persistent(&args.path);
    let runtime = RedDBRuntime::with_options(opts).map_err(|err| format!("open db: {err}"))?;

    let pager = runtime
        .db()
        .store()
        .pager()
        .cloned()
        .ok_or_else(|| "vault requires a paged database (persistent mode)".to_string())?;

    // AuthConfig defaults are fine — we only need the vault wired up.
    // Bootstrap doesn't depend on `enabled = true` because needs_bootstrap()
    // checks the user table directly, not the AuthConfig flag.
    let config = AuthConfig {
        vault_enabled: true,
        ..AuthConfig::default()
    };

    let store =
        AuthStore::with_vault(config, pager, None).map_err(|err| format!("open vault: {err}"))?;

    if !store.needs_bootstrap() {
        // Flush so the freshly-opened pager doesn't leave stray pages
        // behind on disk; we still error out non-zero.
        let _ = runtime.checkpoint();
        return Err("already bootstrapped — bootstrap is one-shot and irreversible".into());
    }

    let result = store
        .bootstrap(&args.username, &password)
        .map_err(|err| format!("bootstrap: {err}"))?;

    let certificate = result.certificate.clone().ok_or_else(|| {
        "bootstrap succeeded but no certificate was issued (vault not configured?)".to_string()
    })?;
    let api_key = result.api_key.key.clone();

    // Vault::save() inside bootstrap() already calls pager.flush()
    // and writes the vault pages directly. We deliberately do NOT
    // call runtime.checkpoint() here because the runtime checkpoint
    // path can rewrite reserved pages (vault occupies pages 2-3,
    // which the engine treats as off-limits during normal commit but
    // may touch during a fresh checkpoint on a brand-new file).
    // Dropping the runtime closes file handles cleanly.
    drop(store);
    drop(runtime);

    Ok(BootstrapOutcome {
        username: result.user.username,
        api_key,
        certificate,
    })
}

/// Resolve the password from `--password-stdin`, `--password`, or
/// `REDDB_PASSWORD` (already populated by the *_FILE expansion at
/// boot). Order: stdin > flag > env.
fn resolve_password(args: &BootstrapArgs) -> Result<String, String> {
    if args.password_stdin {
        let mut buf = String::new();
        let stdin = std::io::stdin();
        stdin
            .lock()
            .read_line(&mut buf)
            .map_err(|err| format!("read password from stdin: {err}"))?;
        // Strip the line-ending; preserve any internal whitespace
        // (passwords like `   ` are unusual but legal).
        let trimmed = buf
            .trim_end_matches(|c: char| c == '\n' || c == '\r')
            .to_string();
        return Ok(trimmed);
    }
    if let Some(p) = args.password.as_ref() {
        let _ = writeln!(
            std::io::stderr(),
            "warning: --password leaks credentials to /proc/<pid>/cmdline; prefer --password-stdin or REDDB_PASSWORD_FILE"
        );
        return Ok(p.clone());
    }
    if let Ok(env_pwd) = std::env::var("REDDB_PASSWORD") {
        if !env_pwd.is_empty() {
            return Ok(env_pwd);
        }
    }
    Ok(String::new())
}

/// Render the outcome to stdout, honouring the requested format. This
/// is the only place the dispatcher prints success output.
pub fn render_success(outcome: &BootstrapOutcome, args: &BootstrapArgs) {
    if args.json {
        // Hand-built JSON — we already do the same in red.rs and adding
        // serde_json round-trip here would pull a dep we don't have.
        println!(
            "{{\"username\":\"{}\",\"token\":\"{}\",\"certificate\":\"{}\"}}",
            json_escape(&outcome.username),
            json_escape(&outcome.api_key),
            json_escape(&outcome.certificate),
        );
        return;
    }
    if args.print_certificate {
        // Just the cert — useful for `cert=$(red bootstrap ... --print-certificate)`.
        println!("{}", outcome.certificate);
        return;
    }
    eprintln!(
        "[reddb] bootstrapped admin user `{}` — SAVE THIS CERTIFICATE (only way to unseal):",
        outcome.username
    );
    println!("{}", outcome.certificate);
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_flag_required() {
        let args = BootstrapArgs {
            path: PathBuf::from("/tmp/reddb-bootstrap-test.rdb"),
            vault: false,
            username: "admin".into(),
            password: Some("hunter2".into()),
            password_stdin: false,
            print_certificate: false,
            json: false,
        };
        let err = run(args).unwrap_err();
        assert!(err.contains("--vault"), "got: {err}");
    }

    #[test]
    fn json_escape_handles_control_chars() {
        assert_eq!(json_escape("a\"b"), "a\\\"b");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
        assert_eq!(json_escape("x\n"), "x\\n");
        assert_eq!(json_escape("\t"), "\\t");
    }
}
