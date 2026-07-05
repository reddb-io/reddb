# Config, Secrets, and Vault Design

Status: design contract from security review.

This document defines how RedDB configuration, vault secrets, native `SECRET`
columns, native `PASSWORD` columns, backup, restore, break-glass reveal, and
change events should work together.

## 1. Domain Model

RedDB has separate stores with separate semantics.

| Concept | Store | Purpose | External read behavior |
|:--|:--|:--|:--|
| Config | Config KV store | Non-secret operational and application settings | Plaintext |
| Secret | Encrypted vault | Secrets used by RedDB or applications | Masked as `***` |
| Secret reference | Config value pointing to a vault path | Lets config refer to a secret without copying it | Shows reference in `SHOW CONFIG`, masks in value projection |
| Native `SECRET` column | User table value encrypted with vault key | Field-level encrypted application data | Decrypts only for internal evaluation, masks when required |
| Native `PASSWORD` column | User table value hashed with Argon2id | Irreversible password storage | Always masked |

Reserved RedDB namespaces:

- `red.config.*` is reserved for RedDB-owned config.
- `red.secret.*` is reserved for RedDB-owned secrets.

Canonical system storage uses protected keyed collections:

- `red.config` is the system Config collection.
- `red.vault` is the system Vault collection.

Legacy aliases such as `$config.*`, `$secret.*`, and `red.secret.*` are
compatibility-only. Internally they normalize to explicit targets such as
`config:red.config/<key>` and `vault:red.vault/<key>` for policy and audit.
New APIs should use the explicit Config and Vault surfaces.

User namespaces are free-form paths such as `mycompany.payments.stripe.key`.
The command determines the target store:

```sql
SET CONFIG mycompany.flags.beta = true;
SET SECRET mycompany.payments.stripe.key = 'sk_live_...';
```

Reserved namespaces must not be written to the wrong store:

```sql
SET CONFIG red.secret.mtls.private_key = '...'; -- reject
SET SECRET red.config.backup.enabled = true;    -- reject
```

## 2. SQL Surface

Config writes:

```sql
SET CONFIG red.config.backup.enabled = true;
SET CONFIG mycompany.C.Z.W = 123;
DELETE CONFIG mycompany.C.Z.W;
SET CONFIG mycompany.C.Z.W = NULL; -- delete
```

Secret writes:

```sql
SET SECRET red.secret.ai.providers.openai.tokens.default = 'sk_...';
SET SECRET mycompany.payments.stripe.key = 'sk_...';
DELETE SECRET mycompany.payments.stripe.key;
SET SECRET mycompany.payments.stripe.key = NULL; -- delete
```

Secret references in config:

```sql
SET SECRET mycompany.payments.stripe.key = 'sk_...';
SET CONFIG mycompany.payments.api_key = &mycompany.payments.stripe.key;
```

Variable reads:

```sql
SELECT $red.config.backup.enabled;
SELECT $config.mycompany.C.Z.W;

SELECT $red.secret.ai.providers.openai.tokens.default; -- returns ***
SELECT $secret.mycompany.payments.stripe.key; -- returns ***
```

Rules:

- `$red.config.*` reads config.
- `$config.*` reads config.
- `$red.secret.*` reads a vault secret.
- `$secret.*` reads a vault secret.
- `$mycompany.foo` does not perform global lookup.
- Projection of a secret or expression directly derived from a secret returns
  `***`.
- Predicates, assignments, and approved functions may use the real secret value
  internally.
- Comparisons involving a secret may return boolean results.
- Functions that transform or concatenate a secret must not return plaintext.

`SHOW SECRETS [prefix]` and `SHOW SECRET path` return metadata only:

```text
key                         value   status             version
mycompany.payments.key      ***     active             7
legacy.missing.key          ***     restore_failed     2
```

`SHOW CONFIG` may show secret references as references:

```text
mycompany.payments.api_key  &mycompany.payments.stripe.key
```

In a restricted policy mode, secret reference paths may be masked as `&***`.

## 3. Native SECRET and PASSWORD Columns

`SET SECRET path = value` stores a vault secret.

`SECRET('literal')` is for native `SECRET` columns and is encrypted with the
vault field-encryption key.

`PASSWORD('literal')` is for native `PASSWORD` columns and is hashed
irreversibly.

When a vault secret is assigned to a native `SECRET` column, the executor must
encrypt it as a native `SECRET` value:

```sql
SET SECRET red.secret.bootstrap.token = 'sk_123';

UPDATE accounts
SET api_token = $red.secret.bootstrap.token
WHERE id = 1;
```

If `api_token` is typed as `SECRET`, the stored value is ciphertext, not
plaintext.

When a vault secret is assigned to a native `PASSWORD` column, the executor must
hash it as a native `PASSWORD` value.

## 4. Fingerprints

Native `SECRET` columns use random nonces, so direct ciphertext equality is not
suitable for lookup. RedDB should provide `SECRET_FINGERPRINT()` as an official
primitive:

```sql
CREATE TABLE api_tokens (
  id TEXT,
  token SECRET,
  token_fp TEXT
);

SET SECRET incoming.token = 'sk_123';

INSERT INTO api_tokens (id, token, token_fp)
VALUES (
  't1',
  $secret.incoming.token,
  SECRET_FINGERPRINT($secret.incoming.token)
);

UPDATE api_tokens
SET revoked = true
WHERE token_fp = SECRET_FINGERPRINT($secret.incoming.token);
```

Fingerprints must be HMAC-based with a vault key separate from
`red.secret.aes_key`:

- `red.secret.aes_key` encrypts native `SECRET` values.
- `red.secret.fingerprint_key` generates lookup fingerprints.

`red.secret.fingerprint_key` is stable by default. Rotation requires an explicit
migration that recomputes fingerprints.

## 5. Performance Contract

Config and secret lookups must be hot-path operations.

Required runtime model:

- Load config into an in-memory `ConfigStore` snapshot.
- Load vault secrets into an in-memory `SecretStore` snapshot after unseal.
- Use O(1) path lookup. Do not scan `red_config` on `$config`, `CONFIG()`, or
  internal RedDB config reads.
- Store config and secret snapshots behind lock-light copy-on-write publishing,
  such as `ArcSwap` or an equivalent mechanism.
- Resolve constant `$config.*` and `$secret.*` refs once per query execution,
  not once per row.
- Prepared statements and cached plans keep path handles, not frozen values.
  Values are re-resolved at execution start.
- A long-running query sees a snapshot captured at execution start. Later
  `SET CONFIG` or `SET SECRET` calls affect later queries only.

Secrets may be decrypted and kept in process memory after vault unseal. This is
the performance tradeoff. The vault protects at rest; it does not protect
against a compromised live process or memory dump.

## 6. Durability Contract

Config and secret writes must be logical upserts.

```sql
SET CONFIG app.mode = 'old';
SET CONFIG app.mode = 'new';
SELECT $config.app.mode; -- new

SET SECRET app.key = 'old';
SET SECRET app.key = 'new';
```

Public readers see the current value only. History, if needed, belongs in audit
or a separate history store, not in conflicting "first row wins" and "last row
wins" readers.

If a vault-enabled mutation cannot persist to the vault, the operation must
return an error and must not publish the new value as observable state.

This applies to `SET SECRET` and should also apply to auth mutations in
vault-enabled mode. In-memory success with failed vault persistence is not a
valid durability contract.

`SET CONFIG` and `SET SECRET` are persistent global operations. They should be
prohibited inside DML transactions until the config/vault stores participate in
the same transaction/WAL contract.

## 7. Authorization

The default installation may remain permissive. The system must still expose
policy resources and actions so deployments can lock down config and secret
operations.

Suggested resources:

```text
config:red.config/backup.enabled
config:mycompany.flags.beta
vault:red.vault/mtls.private_key
vault:mycompany.payments.stripe.key
```

Suggested actions:

```text
config:read
config:list
config:set
config:delete
secret:use
secret:list
secret:set
secret:delete
secret:reveal
```

`secret:use` and `secret:reveal` are different privileges:

- `secret:use` lets the executor use the secret internally.
- `secret:reveal` allows break-glass plaintext export.

Projection still returns `***` even when the caller can use the secret.

## 8. Secret References

`&path` is a secret reference syntax. It is valid as a config value.

```sql
SET CONFIG red.config.tls.private_key = &red.secret.mtls.private_key;
SET CONFIG mycompany.payments.api_key = &mycompany.payments.stripe.key;
```

Normal interactive writes should validate that the referenced secret exists.
Restore is the exception: it may create unresolved placeholders so the restored
inventory is visible.

`$config.x` should resolve a `SecretRef` automatically for internal use:

```sql
SET SECRET mycompany.revocation_token = 't';
SET CONFIG app.revocation_token = &mycompany.revocation_token;

UPDATE tokens
SET revoked = true
WHERE token = $config.app.revocation_token;
```

This requires both config access and secret-use authorization in restricted
policy modes.

## 9. Backup and Restore

Backups always include configs and secrets.

- Configs are exported in plaintext.
- Secrets are exported as encrypted envelopes, never plaintext.
- Physical backups preserve vault ciphertext inside the database file.

Logical secret export should use an explicit envelope, separate from the live
vault page format. The current JSONL dump format uses one encrypted vault-KV
record for the full KV snapshot:

```json
{
  "kind": "reddb.vault_kv.v1",
  "encrypted": true,
  "keys": ["mycompany.payments.stripe.key"],
  "blob": "52445658..."
}
```

The `keys` list is a plaintext manifest so restore can create placeholders if
the encrypted blob cannot be decrypted. Secret values only exist inside `blob`.
The blob uses AES-256-GCM with AAD `reddb-vault-logical-export-v1` and the
certificate-derived vault export key. The source vault salt remains in the v1
envelope for framing compatibility; restore decrypts with `REDDB_CERTIFICATE`
or `REDDB_CERTIFICATE_FILE`.

Restore imports configs and secrets. If secret envelope decryption fails, the
restore should still restore data and configs. The failed secret becomes an
unresolved placeholder:

```text
path = mycompany.payments.stripe.key
status = restore_failed
usable = false
value = false
```

Using an unresolved secret fails:

```sql
UPDATE tokens SET x = $secret.mycompany.payments.stripe.key;
-- error: secret exists but is unresolved after restore
```

Critical `red.secret.*` placeholders may block startup only when the subsystem
that depends on them is enabled. Examples:

- `red.secret.aes_key` is critical for native `SECRET` columns.
- mTLS private key is critical only when that listener depends on it.
- backup credentials are critical only for the backup subsystem.

Restore should publish config and secret restore events after import.

## 10. Internal API Consumers

Any RedDB subsystem that consumes a secret stored through RedDB must use the
vault-backed `SecretResolver`. It must not read plaintext from `red_config`.

Initial consumers:

- AI provider credentials.
- mTLS private keys and certificate material when configured via RedDB.
- Backup backend credentials when configured via RedDB.
- Replication/shared HMAC or API tokens when configured via RedDB.
- Webhook/outbound HTTP credentials when present.

AI credential storage contract:

```text
red.secret.ai.providers.<provider>.tokens.<alias>
red.config.ai.providers.<provider>.tokens.<alias>.secret_ref = 'red.secret.ai.providers.<provider>.tokens.<alias>'
red.config.ai.<provider>.<alias>.base_url = 'https://...'
red.config.ai.default.provider = 'openai'
red.config.ai.default.model = '...'
```

`/ai/credentials` must write API keys to the vault and write only a secret
reference into config. If called without a vault and the payload includes an
API key, the endpoint must fail. Non-secret AI config, such as base URL or
default model, may still be accepted.

Environment variables may remain as operational overrides, but RedDB-managed
credentials are stored in the vault.

## 11. Secret Resolver

All secret use should go through one internal interface:

```rust
trait SecretResolver {
    fn resolve_for_use(&self, path: &str, ctx: ResolveContext) -> Result<ResolvedSecret>;
    fn metadata(&self, path: &str) -> Result<SecretMetadata>;
}
```

Resolution context should include:

```rust
struct ResolveContext {
    principal: Option<UserId>,
    tenant: Option<String>,
    purpose: SecretPurpose,
    trusted_internal: bool,
}
```

Suggested purposes:

```rust
enum SecretPurpose {
    AiProviderKey,
    TlsPrivateKey,
    BackupCredential,
    ReplicationCredential,
    SqlExpression,
    BreakGlassReveal,
}
```

The resolver owns authorization, masking, unresolved errors, audit hooks,
metrics, and hot snapshot lookup.

## 12. Break-Glass Reveal

Secret plaintext reveal must be possible, but it must not be available through
ordinary SQL projection, `SHOW SECRET`, or normal `GET /secrets`.

Recommended reveal surface:

```bash
red secret reveal mycompany.payments.stripe.key \
  --admin-token-file /run/secrets/red_admin_token \
  --vault-certificate-file /run/secrets/reddb_certificate \
  --reason "incident INC-123 rotate provider credential" \
  --out /tmp/stripe.key \
  --mode 0600
```

Rules:

- No SQL reveal.
- No ordinary HTTP `reveal=true` endpoint.
- Requires admin/operator authorization.
- Requires proof of possession of the vault certificate or equivalent recovery
  material.
- Requires a reason.
- Writes audit event with value masked.
- Prints to stdout only with an explicit `--stdout` flag.
- May be disabled by policy/config.
- Revealing a secret marks it as `needs_rotation=true` by default.

Admin token alone is not enough. Certificate alone is not enough. Reveal should
use dual control.

`red.secret.*` internal secrets are blocked by default from reveal, except for
an explicit allowlist. Revealing `red.secret.aes_key` or
`red.secret.fingerprint_key` should be prohibited by default.

## 13. Secret Reveal Reviews

The first reveal of a secret must create a review record in addition to the
immutable audit event.

Review records should live outside the vault, because they do not contain
secret values and must be searchable:

```text
red_secret_reviews
```

Fields:

```text
review_id
review_type
secret_path
status
reason
principal
created_at
revealed_at
needs_rotation
rotation_deadline
closed_at
closed_by
close_reason
```

Review types:

- `secret_reveal`
- `secret_restore_failed`

Reveal creates an open review with `needs_rotation=true`. Restore failure
creates an open review requiring the operator to reimport or recreate the
secret.

Rotating a secret closes open reveal reviews as `rotated` and open restore
failure reviews as `resolved`. Deleting a secret closes related reviews as
`deleted`.

## 14. Secret Metadata and Rotation Policy

Minimum metadata:

```text
created_at
updated_at
version
status
needs_rotation
last_revealed_at
last_rotation_event_status
last_rotation_event_error
```

Useful phase-two metadata:

```sql
ALTER SECRET mycompany.payments.stripe.key
SET owner = 'payments',
    description = 'Stripe production API key',
    rotation_interval_days = 90,
    rotation_event_queue = 'payments_secret_events';
```

Expired or rotation-due secrets should not block usage by default. They should
show up in `SHOW SECRETS`, metrics, and admin status. Policy may enforce stricter
blocking.

## 15. Event Queues

Config and secret changes publish messages to normal RedDB queues.

Default queues:

```text
red_secret_events
red_config_events
```

These queues are regular RedDB queues so consumers can use existing queue
commands, consumer groups, ack/nack, retry, DLQ, and policies.

Secret events:

- `secret.created`
- `secret.rotated`
- `secret.deleted`
- `secret.revealed`
- `secret.restore_failed`
- `secret.recovered`
- `secret.rotation_due`
- `secret.restore_imported`

Config events:

- `config.created`
- `config.updated`
- `config.deleted`
- `config.secret_ref_changed`
- `config.restore_imported`

Secret event payloads never include secret values:

```json
{
  "event_id": "secret:mycompany.payments.stripe.key:v7:rotated",
  "type": "secret.rotated",
  "path": "mycompany.payments.stripe.key",
  "version": 7,
  "status": "active",
  "needs_rotation": false,
  "changed_at": "...",
  "review_ids_closed": ["SRR-123"],
  "value": "***"
}
```

Config event payloads may include plaintext config values, except when the
config value is a secret reference:

```json
{
  "event_id": "config:mycompany.payments.api_key:v2:secret_ref_changed",
  "type": "config.secret_ref_changed",
  "path": "mycompany.payments.api_key",
  "version": 2,
  "secret_ref": "&mycompany.payments.stripe.key",
  "value": "***"
}
```

Publication order:

1. Validate authorization.
2. Persist durable store.
3. Publish new in-memory snapshot.
4. Invalidate caches.
5. Publish event to the queue.
6. Return success or partial success.

If event publication fails after persistence, do not roll back the config or
secret. Return partial success and mark metadata:

```text
last_rotation_event_status = failed
last_rotation_event_error = ...
```

Operators can republish:

```sql
PUBLISH SECRET EVENT mycompany.payments.stripe.key;
PUBLISH CONFIG EVENT mycompany.flags.beta;
```

Events need deterministic idempotency keys so consumers can ignore duplicates.

On restore, publish per-path restore events. Batch events may also be published
for efficiency:

```json
{"type":"secret.restore_batch_imported","count":500,"restore_id":"..."}
```

## 16. CLI and HTTP

CLI must support safe secret input without shell history:

```bash
red secret set red.secret.ai.providers.openai.tokens.default --stdin
red secret set red.secret.mtls.private_key --file /run/secrets/tls.key
red secret list
red secret delete mycompany.payments.stripe.key
red secret reveal mycompany.payments.stripe.key --reason "..."
```

HTTP `/secrets` should exist, but never return plaintext:

- `PUT /secrets/{path}` sets a typed secret value.
- `DELETE /secrets/{path}` deletes a secret.
- `GET /secrets/{path}` returns metadata with `value: "***"`.
- `GET /secrets?prefix=x` lists metadata.

Plaintext reveal belongs only to the break-glass mechanism.

Config file import may include secret references, not secret values:

```json
{
  "red": {
    "config": {
      "ai": {
        "providers": {
          "openai": {
            "tokens": {
              "default": {
                "secret_ref": {"$secret": "red.secret.ai.providers.openai.tokens.default"}
              }
            }
          }
        }
      }
    }
  }
}
```

Normal config files must not embed secret plaintext.

## 17. First Delivery

Recommended implementation order:

1. Parser and AST:
   - `SET SECRET`
   - `DELETE SECRET`
   - `SHOW SECRET(S)`
   - `$config.path`
   - `$secret.path`
   - `&path` in `SET CONFIG`

2. Stores:
   - in-memory `ConfigStore` snapshot with O(1) lookup
   - in-memory `SecretStore` snapshot with O(1) lookup
   - typed vault values
   - durable error returns

3. SQL semantics:
   - mask secret projection
   - internal secret use in predicates, assignments, and approved functions
   - `SECRET_FINGERPRINT()`
   - fix `UPDATE ... SET secret_col = SECRET('...')`

4. HTTP and CLI:
   - `/secrets`
   - `red secret set/list/delete/reveal`
   - `/ai/credentials` writes keys to vault

5. Backup and restore:
   - config plaintext export/import
   - secret encrypted envelopes
   - unresolved placeholders and review records on secret restore failure

6. Event queues:
   - `red_secret_events`
   - `red_config_events`
   - deterministic event ids
   - republish commands

7. Documentation:
   - config vs secret model
   - SQL syntax
   - break-glass reveal and review lifecycle
   - AI credential dogfooding
   - backup and restore behavior
   - event queue contracts
