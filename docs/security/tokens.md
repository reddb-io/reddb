# API Keys & Tokens

RedDB supports two authentication token types: persistent API keys and session tokens.

## API Keys

API keys are persistent credentials tied to a user and role. They don't expire until revoked.

### Create an API Key

```bash
# Via CLI
red auth create-api-key alice --name "ci-token" --role write

# Via HTTP
curl -X POST http://127.0.0.1:8080/auth/api-keys \
  -H 'content-type: application/json' \
  -H 'Authorization: Bearer <admin-token>' \
  -d '{"username": "alice", "name": "ci-token", "role": "write"}'
```

Response:

```json
{
  "ok": true,
  "key": "rdb_k_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "name": "ci-token",
  "role": "write",
  "username": "alice"
}
```

> [!WARNING]
> The full API key is only shown once. Store it securely.

### Using an API Key

```bash
curl http://127.0.0.1:8080/collections/users/scan \
  -H 'Authorization: Bearer rdb_k_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'
```

### Revoking an API Key

```bash
grpcurl -plaintext \
  -H 'Authorization: Bearer <admin-token>' \
  -d '{"payloadJson": "{\"key\":\"rdb_k_xxxx\"}"}' \
  127.0.0.1:50051 reddb.v1.RedDb/AuthRevokeApiKey
```

## Session Tokens

Session tokens are obtained by logging in and expire after the session ends.

### Login

```bash
# Via CLI
red auth login alice --password secret

# Via HTTP
curl -X POST http://127.0.0.1:8080/auth/login \
  -H 'content-type: application/json' \
  -d '{"username": "alice", "password": "secret"}'
```

Response:

```json
{
  "ok": true,
  "token": "rdb_s_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
  "username": "alice",
  "role": "admin"
}
```

### Using a Session Token

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'Authorization: Bearer rdb_s_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' \
  -H 'content-type: application/json' \
  -d '{"query": "SELECT * FROM users"}'
```

## Token Prefixes

| Prefix | Type |
|:-------|:-----|
| `rdb_k_` | API Key |
| `rdb_s_` | Session Token |
