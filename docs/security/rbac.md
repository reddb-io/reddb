# Users & Roles (RBAC)

RedDB uses role-based access control with three roles: `admin`, `write`, and `read`.

## Creating Users

```bash
# Via CLI
red auth create-user alice --password secret --role admin

# Via HTTP
curl -X POST http://127.0.0.1:8080/auth/users \
  -H 'content-type: application/json' \
  -H 'Authorization: Bearer <admin-token>' \
  -d '{"username": "alice", "password": "secret", "role": "write"}'
```

## Listing Users

```bash
# Via CLI
red auth list-users

# Via HTTP
curl http://127.0.0.1:8080/auth/users \
  -H 'Authorization: Bearer <admin-token>'
```

## Role Permissions

| Operation | `read` | `write` | `admin` |
|:----------|:-------|:--------|:--------|
| Query (SELECT) | Yes | Yes | Yes |
| Scan | Yes | Yes | Yes |
| Health/Stats | Yes | Yes | Yes |
| Insert entities | No | Yes | Yes |
| Update entities | No | Yes | Yes |
| Delete entities | No | Yes | Yes |
| Create collections | No | Yes | Yes |
| Drop collections | No | No | Yes |
| Manage users | No | No | Yes |
| Manage API keys | No | No | Yes |
| Snapshots/Exports | No | No | Yes |
| Index management | No | No | Yes |

## Changing Passwords

```bash
curl -X POST http://127.0.0.1:8080/auth/change-password \
  -H 'content-type: application/json' \
  -H 'Authorization: Bearer <token>' \
  -d '{"username": "alice", "password": "new-secret"}'
```

## Deleting Users

```bash
grpcurl -plaintext \
  -H 'Authorization: Bearer <admin-token>' \
  -d '{"payloadJson": "{\"username\":\"alice\"}"}' \
  127.0.0.1:50051 reddb.v1.RedDb/AuthDeleteUser
```

## Who Am I

Check the current authenticated user:

```bash
curl http://127.0.0.1:8080/auth/whoami \
  -H 'Authorization: Bearer <token>'
```
