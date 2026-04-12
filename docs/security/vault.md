# Vault (Certificate Seal)

The vault stores authentication data (users, roles, API keys) in encrypted reserved pages within the main database file.

## Enabling the Vault

```bash
red server --http --path ./data/reddb.rdb --vault --bind 0.0.0.0:8080
```

## How It Works

1. Reserved pages at the beginning of the database file store auth data
2. Pages are encrypted with AES-256-GCM
3. The encryption key is derived from the database file identity (certificate seal)
4. Auth data is only accessible through the RedDB process

## What's Stored

| Data | Storage |
|:-----|:--------|
| User records | Username, hashed password, role |
| API keys | Key hash, name, role, owner |
| Session tokens | Token hash, expiration, role |
| Encrypted KV store | Arbitrary `red.secret.*` key/value pairs |
| `Secret` column AES key | `red.secret.aes_key` — auto-generated on first boot |

## Secret Namespace

The vault exposes a generic key-value store for arbitrary sensitive data. Keys use the `red.secret.*` prefix so that parsers can route them to the encrypted store instead of the plaintext `red_config` collection.

```text
red.secret.aes_key          # 32-byte AES-256 key used by Value::Secret columns
red.secret.stripe.api_key   # application-defined secret
red.secret.oauth.client     # application-defined secret
```

Compare with `red.config.*` keys — those live in the plaintext `red_config` KV store and are intended for non-sensitive configuration.

## Value::Secret and Value::Password

Column-level types (`Secret`, `Password`) integrate with the vault:

- `Secret` columns are encrypted with the vault's `red.secret.aes_key` on INSERT and decrypted on SELECT when the vault is unsealed. A sealed vault renders `Secret` columns as `***`.
- `Password` columns store an argon2id hash and are **always** returned as `***`. Use `VERIFY_PASSWORD(column, 'candidate')` to compare a plaintext against the stored hash.

See [Primitive Types](/types/primitives.md#secret) for syntax and examples.

## Security Properties

- Auth data is encrypted at rest
- Passwords are never stored in plaintext
- API keys are hashed after initial creation
- The vault key is sealed to the database file

## Without Vault

When `--vault` is not specified:

- Auth endpoints return errors
- All requests are allowed (no authentication)
- Suitable for development and trusted environments

> [!WARNING]
> Without `--vault`, there is no authentication. Any client can read, write, and delete data. Always enable vault for production deployments.
