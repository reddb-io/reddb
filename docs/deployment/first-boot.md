# First Boot Contract

This page records the supported first-boot paths for RedDB delivery modes.
It is intentionally conservative: if a topology cannot safely pick one writer,
auth bootstrap is rejected instead of guessing.

## Boot Phases

Every `red server` boot follows this order:

1. Open storage using the selected storage profile/preset.
2. Seed built-in `red.config` defaults and apply `REDDB_CONFIG_FILE` with
   write-if-absent semantics.
3. Build the auth store. If `--vault` is active, open the encrypted vault from
   `REDDB_CERTIFICATE` or `REDDB_VAULT_KEY` and their `_FILE` companions.
4. If `--no-auth` or `--dev` is active, skip every auth bootstrap path.
5. Otherwise apply `REDDB_BOOTSTRAP_MANIFEST` when set, or `REDDB_PRESET`
   (`simple`, `production`, `regulated`).

`REDDB_PRESET=production` creates the first platform admin, installs an
allow-all policy, attaches it to that admin, and persists
`system.bootstrap.completed`. When vault-backed, the preset prints the newly
issued certificate to stderr; save it and update the runtime secret before any
restart.

For the standalone `red bootstrap` command, always run it against the same
database file/volume that will be served. A certificate minted from a scratch
database does not unseal another database.

## Topology Matrix

| Delivery shape | No-auth first boot | Auth/vault first boot | Current status |
|---|---|---|---|
| Standalone / embedded file | Supported with `--no-auth` or `--dev`; this disables vault and skips presets. | Supported through `red bootstrap --vault` on the real file/volume, or `red server --vault` plus `REDDB_PRESET=production` and admin env. | Supported. |
| Serverless writer | Same as standalone; serverless is a standalone process role with serverless storage/remote backend contract. | Supported on the single writer. Bootstrap the local writer volume before remote backup/restore, or use first-start `REDDB_PRESET=production` and capture the emitted certificate. | Supported for one writer. |
| Primary-replica | Supported on the primary; replicas should not create local admins. | Supported on the primary only. Replicas must receive replicated auth state from the primary and must not receive bootstrap env. | Supported for the primary-replica writer path. |
| Cluster | Supported only as today's cluster-shaped storage/discovery contract with standalone process roles. | Not supported yet. Symmetric members cannot safely pick a first writer, so `mode=cluster` rejects `auth.enabled=true`, and `red server` rejects bootstrap env when the resolved shape is cluster. | Incomplete until cluster writer election/range ownership defines a single auth bootstrap owner. |

## Config First Boot

Initial config comes from three layers:

- Built-in defaults seeded into `red.config` on first boot.
- Critical config-matrix defaults healed on every boot when missing.
- Optional `REDDB_CONFIG_FILE`, mounted by Compose/Helm at
  `/etc/reddb/config.json`, written only when a key is absent.

This means an operator can ship initial config with the container without
overwriting later `SET CONFIG` changes.

## RedDB Cloud Admin Model

RedDB Cloud should use a bootstrap manifest instead of a special
`system_owned` flag. The cloud/head admin and the customer admin are ordinary
platform users. Protection is policy-derived:

- `cloud-admin` receives an allow-all policy.
- `customer-admin` receives an allow-all policy.
- `customer-admin` also receives an explicit deny policy on
  `user:cloud-admin` for:
  - `user:delete`
  - `user:disable`
  - `user:password:change`
  - `user:role:update`

Example manifest shape:

```json
{
  "users": [
    { "username": "cloud-admin", "password": "from-secret", "role": "admin" },
    { "username": "customer-admin", "password": "from-secret", "role": "admin" }
  ],
  "policies": [
    {
      "id": "cloud-admin-allow-all",
      "version": 1,
      "statements": [
        { "effect": "allow", "actions": ["*"], "resources": ["*"] }
      ]
    },
    {
      "id": "customer-admin-allow-all",
      "version": 1,
      "statements": [
        { "effect": "allow", "actions": ["*"], "resources": ["*"] }
      ]
    },
    {
      "id": "protect-cloud-admin",
      "version": 1,
      "statements": [
        {
          "effect": "deny",
          "actions": [
            "user:delete",
            "user:disable",
            "user:password:change",
            "user:role:update"
          ],
          "resources": ["user:cloud-admin"]
        }
      ]
    }
  ],
  "attachments": [
    { "user": "cloud-admin", "policy": "cloud-admin-allow-all" },
    { "user": "customer-admin", "policy": "customer-admin-allow-all" },
    { "user": "customer-admin", "policy": "protect-cloud-admin" }
  ],
  "actor": "cloud-admin"
}
```

Render real password values from the Cloud secret manager into the mounted
manifest at deploy time. Do not commit a manifest containing production
passwords.

The engine rejects `users[].system_owned` in bootstrap manifests and rejects
`condition.system_owned` in policies. Cloud ownership must remain explicit in
user and policy resources.

## Evidence Pointers

- `service_cli::to_db_options` makes `--no-auth` the final word and disables
  vault/presets for that boot.
- `service_cli::apply_preset` applies manifests or presets idempotently via
  `system.bootstrap.completed`.
- `AuthStore::bootstrap` creates the first admin, API key, vault keypair, and
  certificate when a pager-backed vault exists.
- Helm renders auth bootstrap env only into the writer StatefulSet and rejects
  `mode=cluster` with `auth.enabled=true`. The `red server` CLI also rejects
  `REDDB_PRESET`, `REDDB_BOOTSTRAP_MANIFEST`, and admin username/password env
  in cluster-shaped boots unless `--no-auth` or `--dev` is explicit.
- User lifecycle policy gates cover `user:create`, `user:delete`,
  `user:disable`, `user:password:change`, and `user:role:update`; public HTTP
  and gRPC user-delete/password-change paths call that gate.
