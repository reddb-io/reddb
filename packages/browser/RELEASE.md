RedDB Browser 0.8.0 adds a local file-owner Vault for explicitly saved credentials.

`database.vault` exposes initialization status, passphrase unlock, lock, key names,
explicit reveal and encrypted put/delete. Put/delete participate in the same
BEGIN/COMMIT/ROLLBACK publication as ordinary table changes. The native RDVX
Vault envelope and v3 Argon2id passphrase derivation are shared with the server;
no application ciphertext table or JavaScript crypto implementation is involved.

A reusable database session retains its opaque zeroizing Vault key while file
handles close and reopen. Keys are scoped to the database name and persisted
random Vault identity; lock, session disposal and Worker termination discard
them. A browser restart always starts locked. Wrong passwords never initialize
over existing credentials. Ordinary workspace metadata remains available while
locked.

This is a local file-owner credential API, not server IAM, tenant policy,
encrypted-column SQL or synchronization. Full-file backups still contain plain
metadata and can retain deleted encrypted bytes. Applications own opt-in saving,
master-password prompts, cross-tab operation queuing and session-token policy.
