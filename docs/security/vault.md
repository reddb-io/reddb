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
