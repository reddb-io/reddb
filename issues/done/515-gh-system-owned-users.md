# GitHub Issue #515: Engine system_owned flag + user immutability guards

URL: https://github.com/reddb-io/reddb/issues/515

## Build

Add `system_owned: bool` to engine users and prevent destructive user mutation paths from changing system-owned users.

## Acceptance

- `User` has `system_owned: bool` with `#[serde(default)]`.
- `AuthError::SystemUserImmutable { username }` exists.
- Delete, disable/set-enabled, password change, and role-change paths reject system-owned users.
- API key creation/revoke/rotation remain allowed for system-owned users.
- `AuthStore::create_system_user(username, password, role, tenant_id)` exists.
- `POST /v1/_admin/system-users` exists and requires shared-secret auth.
- Tests cover immutability, regular user mutation, old persisted user deserialization, and system-user API-key rotation.
