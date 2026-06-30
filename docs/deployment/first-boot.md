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
5. Otherwise apply `REDDB_BOOTSTRAP_MANIFEST` when set, or the configured
   bootstrap preset.

The bootstrap preset is selected by `--bootstrap-preset` /
`REDDB_BOOTSTRAP_PRESET` (with `REDDB_PRESET` accepted as a legacy alias). The
accepted values are `simple`, `production`, `regulated`, and `cloud`; the
default is `simple`. The `production` and `cloud` presets auto-enable `--auth`,
`--require-auth`, and `--vault` unless `--no-auth` wins.

`--bootstrap-preset production` creates the first platform admin, installs an
allow-all policy, attaches it to that admin, and persists the bootstrap-complete
marker. When vault-backed, the preset prints the newly issued certificate to
stderr; save it and update the runtime secret before any restart.

## Single-Process Bootstrap Then Serve

Distroless images, Fly Machines, and Kubernetes pods without a bootstrap
sidecar can run one `red server` process that bootstraps the real database
volume and then continues serving from it. The boot needs an explicit
first-boot intent: either a preset such as `cloud` / `production` or a
manifest.

```bash
red server \
  --path /data/reddb.rdb \
  --http \
  --http-bind 0.0.0.0:5000 \
  --vault true \
  --bootstrap-preset cloud \
  --cloud-head-admin red_admin \
  --cloud-head-admin-password-file /run/secrets/red_admin_password \
  --customer-admin app_admin \
  --customer-admin-password-file /run/secrets/app_admin_password \
  --bootstrap-cert-out /run/reddb/bootstrap/certificate
```

On a fresh `--path`, `red server` creates the paged vault in place, applies the
bootstrap intent, writes `system.bootstrap.completed`, opens listeners, and
keeps serving. `--bootstrap-cert-out` writes the minted 64-hex unseal
certificate to a file as well as emitting it on stderr. Before the next restart,
copy that file into your secret store and mount it back with:

```bash
REDDB_CERTIFICATE_FILE=/run/secrets/reddb_certificate
```

Re-booting the same database is idempotent. When `system.bootstrap.completed`
is present, the server skips the preset or manifest, does not create another
admin, and does not mint or rewrite the certificate. The certificate file is an
unseal secret for an already-bootstrapped vault; it is not a bootstrap intent by
itself.

For manifest-driven first boot, replace the preset credentials with a mounted
manifest:

```bash
red server \
  --path /data/reddb.rdb \
  --vault true \
  --bootstrap-manifest /run/secrets/reddb-bootstrap-manifest.json \
  --bootstrap-cert-out /run/reddb/bootstrap/certificate
```

The manifest path is read only while applying the first boot. After the
completion marker is durable, a restart can run with the same manifest still
mounted, a different manifest mounted, or no manifest mounted; it will not
re-apply bootstrap state.

Prefer file-backed secrets for unattended containers:

- `--bootstrap-admin-password-file` / `REDDB_PASSWORD_FILE`
- `--cloud-head-admin-password-file` / `REDDB_CLOUD_HEAD_ADMIN_PASSWORD_FILE`
- `--customer-admin-password-file` / `REDDB_CUSTOMER_ADMIN_PASSWORD_FILE`
- `REDDB_CERTIFICATE_FILE` for restarts after bootstrap

Do not bake bootstrap passwords, manifests, or certificates into an image.

## Bootstrap Presets

Presets are policy-first: every user a preset creates is an ordinary,
policy-governed platform user. There is no `system_owned` shortcut — protection
comes from explicit allow/deny statements (see [policy-first bootstrap](#policy-first-bootstrap)).

| Preset | Users created at first boot | Allow-all policy | Auth/vault auto-enabled | Managed guardrails installed |
|---|---|---|---|---|
| `simple` (default) | None | — | No (auth knobs stay as configured) | None |
| `production` | One first admin from `--bootstrap-admin` / `REDDB_USERNAME` (+ password) | Yes, attached to the admin | Yes | `system.bootstrap.first-admin-allow-all` |
| `cloud` | Two: a head/platform admin (`--cloud-head-admin`) and a customer admin (`--customer-admin`) | Yes, attached to both | Yes | `system.cloud.protect-managed`; `red.config.cloud.*` registered managed |
| `regulated` | None | — | Yes | `system.regulated.protect-managed`; audit / evidence / query-audit config registered managed; query-audit infrastructure enabled |

Admin credentials are supplied per preset:

- `production` reads `--bootstrap-admin` / `REDDB_USERNAME` and
  `--bootstrap-admin-password` / `REDDB_PASSWORD` (or the `*-password-file`
  variant).
- `cloud` reads `--cloud-head-admin` / `REDDB_CLOUD_HEAD_ADMIN` and
  `--customer-admin` / `REDDB_CUSTOMER_ADMIN` (each with matching password and
  `-password-file` flags). The two usernames must differ.

The bootstrap-complete marker makes presets idempotent: a preset applies once
and is skipped on every later boot of the same database.

For the standalone `red bootstrap` command, always run it against the same
database file/volume that will be served. A certificate minted from a scratch
database does not unseal another database.

## Topology Matrix

| Delivery shape | No-auth first boot | Auth/vault first boot | Current status |
|---|---|---|---|
| Standalone / embedded file | Supported with `--no-auth` or `--dev`; this disables vault and skips presets. | Supported through `red bootstrap --vault` on the real file/volume, or `red server --vault` plus `REDDB_BOOTSTRAP_PRESET=production` and admin env. | Supported. |
| Serverless writer | Same as standalone; serverless is a standalone process role with serverless storage/remote backend contract. | Supported on the single writer. Bootstrap the local writer volume before remote backup/restore, or use first-start `REDDB_BOOTSTRAP_PRESET=production` and capture the emitted certificate. | Supported for one writer. |
| Primary-replica | Supported on the primary; replicas should not create local admins. | Supported on the primary only. Replicas must receive replicated auth state from the primary and must not receive bootstrap env. | Supported for the primary-replica writer path. |
| Cluster | Supported only as today's cluster-shaped storage/discovery contract with standalone process roles. | Authority model selected: cluster auth/vault/config/policy first boot uses the reserved global system range owner. Runtime support is still incomplete, so `mode=cluster` continues to reject `auth.enabled=true`, and `red server` continues to reject bootstrap env when the resolved shape is cluster unless `--no-auth` or `--dev` is explicit. | Design accepted in [ADR 0058](../../.red/adr/0058-cluster-bootstrap-authority.md); implementation remains tracked by [PRD #1227](https://github.com/reddb-io/reddb/issues/1227). |

## Cluster Bootstrap Authority

Cluster auth/vault/config/policy first boot uses the reserved global system range owner. The reserved range stores global auth, vault, config, policy state, and `system.bootstrap.completed`; no per-node marker or router cache is authoritative for clustered bootstrap completion.

Only the current lease/term owner of that reserved range may run the first
preset, create initial admins, initialize vault material, apply the first
operator/cloud policy manifest, or publish the bootstrap-complete marker. Those
writes must be compare-and-set guarded and idempotent: a restart retries from
durable reserved-range state, rolls matching partial state forward, or rejects a
conflict rather than creating a second global auth state.

Non-owner members must not run presets, create initial admins, or initialize vault material. When bootstrap credentials or a manifest are present, they wait, forward/redirect to the current reserved-range owner, or observe `system.bootstrap.completed` after it commits.

Anonymous `--no-auth` / `--dev` cluster-shaped boot remains an explicit development carveout, not a production bootstrap path. RedDB Cloud keeps the policy-first manifest model: the reserved range owner applies the cloud/operator manifest as the initial global policy source.

### Helm and Compose Cluster Delivery

The Helm chart (`charts/reddb`) and the Compose example
(`examples/docker-compose.cluster.yml`) render this contract directly, and
`scripts/verify-helm-chart.sh` asserts it on every render:

- **No-auth cluster (supported today).** Both deliveries render cluster members
  as symmetric non-owners with *no* bootstrap credentials
  (`REDDB_PRESET` / `REDDB_USERNAME` / `REDDB_PASSWORD` /
  `REDDB_BOOTSTRAP_MANIFEST` are never emitted into a cluster pod). Members boot
  anonymously and write no admin, vault, or completion marker.
- **Auth/vault cluster (gated).** `auth.enabled=true` is rejected fail-closed in
  `mode=cluster` (`helm template` fails with an authority-aware message), in
  lockstep with the runtime seam, until the reserved-range owner path lands
  (PRD #1227). For a credentialled deploy today, bootstrap auth on a single-owner
  topology (standalone/serverless/primary) — e.g. `red bootstrap --vault`
  against the real volume as in `examples/docker-compose.vault.yml`.
- **Certificate handling.** A cluster member may still receive the vault
  certificate (env or `fileMount`) to *unseal* an already-bootstrapped store on
  restart; receiving a certificate never bootstraps auth. Operators capture the
  certificate the owner mints once and preserve it offline.
- **Restart idempotency.** A restart that observes `system.bootstrap.completed`
  rehydrates read-only state only, so re-running `helm upgrade` or
  `docker compose up` recreates no admin and reissues no certificate.
- **Non-owner behavior.** Because members never receive credentials, no member
  can mutate global auth state; the owner is the only writer once the runtime
  owner path exists.

## Config First Boot

Initial config comes from three layers:

- Built-in defaults seeded into `red.config` on first boot.
- Critical config-matrix defaults healed on every boot when missing.
- Optional `REDDB_CONFIG_FILE`, mounted by Compose/Helm at
  `/etc/reddb/config.json`, written only when a key is absent.

This means an operator can ship initial config with the container without
overwriting later `SET CONFIG` changes.

## Policy-First Bootstrap

Bootstrap and manifest users are ordinary policy-governed principals. There is
no special operator-owned class:

- The engine rejects `users[].system_owned` in bootstrap manifests and rejects
  `condition.system_owned` in policies. Ownership must be modeled as explicit
  `user:*` / `policy:*` allow and deny statements.
- User-lifecycle mutations authorize against `user:*` actions
  (`user:create`, `user:update`, `user:disable`, `user:delete`,
  `user:password:change`, `user:role:update`) on `user:<username>` resources.
  Public HTTP and gRPC user-delete / password-change paths call that gate.
- Managed policies are protected by policy, not by a structural flag: once IAM
  is active, `policy:put` / `policy:drop` / `policy:attach` / `policy:detach`
  on a managed policy resource must be explicitly allowed, and an explicit Deny
  always wins. The `production`, `cloud`, and `regulated` presets install their
  guardrails as managed policies governed this way.

## RedDB Cloud Admin Model

RedDB Cloud should use a bootstrap manifest instead of a special
`system_owned` flag. The cloud/head admin and the customer admin are ordinary
platform users. Protection is policy-derived:

- `red_admin` (or the configured cloud/head admin username) receives an
  allow-all policy.
- `customer-admin` receives an allow-all policy.
- `customer-admin` also receives an explicit deny policy on
  `user:red_admin` for:
  - `user:delete`
  - `user:disable`
  - `user:password:change`
  - `user:role:update`
- The protective policy also denies dropping or detaching the guardrail policy
  itself, and denies replacing or reattaching it, so the customer admin cannot
  remove the policy before deleting the head admin.

Example manifest shape:

```json
{
  "users": [
    { "username": "red_admin", "password": "from-secret", "role": "admin" },
    { "username": "customer-admin", "password": "from-secret", "role": "admin" }
  ],
  "policies": [
    {
      "id": "red-admin-allow-all",
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
      "id": "system.cloud.protect-managed",
      "version": 1,
      "statements": [
        {
          "effect": "deny",
          "actions": ["policy:put", "policy:drop", "policy:attach", "policy:detach"],
          "resources": ["policy:system.cloud.protect-managed"]
        },
        {
          "effect": "deny",
          "actions": [
            "user:delete",
            "user:disable",
            "user:password:change",
            "user:role:update"
          ],
          "resources": ["user:red_admin"]
        }
      ]
    }
  ],
  "attachments": [
    { "user": "red_admin", "policy": "red-admin-allow-all" },
    { "user": "customer-admin", "policy": "customer-admin-allow-all" },
    { "user": "customer-admin", "policy": "system.cloud.protect-managed" }
  ],
  "actor": "red_admin"
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
- `service_cli` resolves the bootstrap preset from `--bootstrap-preset`,
  `REDDB_BOOTSTRAP_PRESET`, then the legacy `REDDB_PRESET`, defaulting to
  `simple`, and applies manifests or presets idempotently via the
  bootstrap-complete marker.
- `AuthStore::bootstrap` creates the first admin, API key, vault keypair, and
  certificate when a pager-backed vault exists.
- Helm renders auth bootstrap env only into the writer StatefulSet and rejects
  `mode=cluster` with `auth.enabled=true`. The `red server` CLI also rejects
  `REDDB_BOOTSTRAP_PRESET` / `REDDB_PRESET`, `REDDB_BOOTSTRAP_MANIFEST`, and
  admin username/password env in cluster-shaped boots unless `--no-auth` or
  `--dev` is explicit.
- User lifecycle policy gates cover `user:create`, `user:delete`,
  `user:disable`, `user:password:change`, and `user:role:update`; public HTTP
  and gRPC user-delete/password-change paths call that gate.
