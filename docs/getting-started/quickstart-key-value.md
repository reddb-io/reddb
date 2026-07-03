# Quickstart: Key-Value, Config & Vault

The **key-value** model is the simplest semantic layer over a `collection`
(the universal container): opaque `key -> value` pairs. The same shape powers
config flags and, with a vault-backed collection, secrets.

## 1. Start RedDB

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Connect with `red connect 127.0.0.1:55055` (or POST to
`http://127.0.0.1:5000/query`).

## 2. Write key-value pairs

```sql
INSERT INTO settings KV (key, value) VALUES ('feature_flag', true);
INSERT INTO settings KV (key, value) VALUES ('max_retries', 3);
```

Values keep their type — a boolean stays a boolean, a number stays a number.

## 3. Your first meaningful result

Read the collection back, ordered by key:

```sql
SELECT key, value FROM settings ORDER BY key ASC;
```

```text
 key          | value
--------------+------
 feature_flag | true
 max_retries  | 3
```

## Where to go next

- [Key-Value](/data-models/key-value.md) — the full KV model
- [Cache](/data-models/cache.md) — KV with TTL and invalidation
- [Vault & secrets](/security/vault.md) — encrypted, access-controlled values
