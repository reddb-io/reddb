//! Default permissions for the files RedDB creates.
//!
//! A database writes a lot of durable state: the `.rdb` itself, WAL
//! segments, the double-write buffer, the audit log, exports, backups,
//! diagnostic logs. Only a handful of paths set an explicit mode
//! (`storage::keyring`, the TLS key writers, `service_cli`'s cert output);
//! everything else inherited the process umask, which on a typical Linux
//! image is `022` — world-readable.
//!
//! That is the wrong default for a file holding user rows, WAL frames that
//! replay those rows, or an audit trail. Any local account could read the
//! database, and the effort to fix it per-call-site is unbounded: every
//! future writer would have to remember.
//!
//! [`restrict_new_file_permissions`] sets the process umask to `0o077`
//! once at startup, so anything RedDB creates from then on is owner-only
//! unless a call site explicitly widens it. It is deliberately a startup
//! concern of the `red` binary rather than a library one: umask is
//! process-global, and a library has no business changing it underneath an
//! embedder.

/// Restrict the process umask so newly created files and directories are
/// owner-only (`0600` / `0700`).
///
/// Returns the previous umask. Call once, early in `main`, before any
/// file is created. No-op on non-Unix targets, where the permission model
/// differs and directory ACLs are the operator's tool.
#[cfg(unix)]
pub fn restrict_new_file_permissions() -> u32 {
    // SAFETY: `umask` is always safe to call; it cannot fail and returns
    // the previous mask. Called before any threads are spawned, so the
    // process-global change is not racing another file creation.
    let previous = unsafe { libc::umask(0o077) };
    previous as u32
}

/// Non-Unix builds keep the platform default.
#[cfg(not(unix))]
pub fn restrict_new_file_permissions() -> u32 {
    0
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    /// The umask is process-global, so this test both sets and restores it,
    /// and is the only test that touches it.
    #[test]
    fn new_files_are_owner_only_after_restricting_the_umask() {
        let previous = super::restrict_new_file_permissions();

        let dir = std::env::temp_dir().join(format!("reddb-umask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("durable.rdb");
        std::fs::write(&path, b"rows").expect("write");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        // Restore before asserting so a failure does not leak the umask into
        // the rest of the process.
        unsafe { libc::umask(previous as libc::mode_t) };
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            mode, 0o600,
            "a file created after restricting the umask must not be group- or world-readable"
        );
    }
}
