# PRD: KV, Config, and Vault are distinct keyed collection models [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/314

Labels: enhancement

GitHub issue number: #314

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Problem Statement

RedDB has been treating "KV", "config", and "secrets/vault" as adjacent concepts, and in some places as if they were one generalized KV surface. That is the wrong product contract.

Normal KV is Redis-flavor application data. It is allowed to be volatile, hot, tagged, expired, incremented, merged, watched, and invalidated in bulk.

Config is stable operational configuration. It should be typed, auditable, rollback-friendly, and protected from accidental volatility. TTL, counters, and destructive invalidation are not valid Config operations.

Vault is sealed secret storage. It must never leak plaintext through normal reads, watch events, list output, backups, WAL, or snapshots. Plaintext access is an explicit unseal action with stronger permission and mandatory audit.

Without a formal split, agents and contributors can easily implement TTL, INCR, or normal GET semantics in the wrong place.

## Solution

Make `KV`, `CONFIG`, and `VAULT` three formal keyed Collection models. They share key-addressed storage mechanics, but not capabilities or safety contracts.

The canonical DDL is:

```sql
CREATE KV sessions;
CREATE CONFIG app;
CREATE VAULT prod;
```

The canonical operation shape is:

```sql
PUT KV sessions user:123 = {...};
GET CONFIG app feature.checkout;
GET VAULT prod stripe.secret_key;
UNSEAL VAULT prod stripe.secret_key;
```

`red.config` and `red.vault` are protected system collections created by bootstrap. Legacy pseudo-paths such as `$config.*`, `$secret.*`, and `red.secret.*` normalize internally to those explicit collections, but docs and new APIs should use the explicit model.

## User Stories

1. As a developer, I want normal KV to support PUT/GET/DELETE/INCR/DECR/CAS/WATCH/TTL/TAGS, so that Redis-shaped application data remains ergonomic.
2. As an operator, I want Config to reject TTL and counter operations, so that stable settings do not disappear or mutate like counters.
3. As an operator, I want Config changes to keep history, so that I can roll back a bad production setting.
4. As a developer, I want Config values to be typed or schema-validated, so that invalid operational settings fail before rollout.
5. As a security engineer, I want Vault values encrypted before WAL/page/snapshot persistence, so that backups and storage media never contain plaintext secrets.
6. As a security engineer, I want `GET VAULT` to return redacted metadata only, so that dashboards and logs do not leak secrets.
7. As a security engineer, I want plaintext secret reads to require `UNSEAL VAULT`, so that unseal intent is explicit and auditable.
8. As a security engineer, I want Vault unseal, purge, key attach, and failed privileged attempts audited without plaintext or ciphertext.
9. As an operator, I want Config to store `SecretRef` values that point to Vault, so that stable config can reference secrets without embedding them.
10. As a developer, I want resolving a `SecretRef` to be explicit, so that normal config reads cannot accidentally leak a secret.
11. As an operator, I want `red.config` and `red.vault` as system collections, so that engine-managed settings and secrets are discoverable but protected.
12. As a driver author, I want separate `db.kv()`, `db.config()`, and `db.vault()` clients, so that autocomplete and permissions do not blur domains.
13. As an API user, I want HTTP/MCP endpoints separated by domain, so that Vault unseal is not hidden behind a generic KV endpoint.
14. As an observer, I want `WATCH VAULT` to emit metadata only, so that change streams do not leak secret material.
15. As an operator, I want tags on Vault and Config to be visible metadata, so that I can group and rotate/list safely without treating tags as secret.
16. As an operator, I want destructive tag invalidation to remain KV-only, so that Config and Vault require explicit safe operations.
17. As a backup operator, I want Vault backups to contain sealed blobs only, so that restore without key material does not expose secrets.
18. As a backup operator, I want restore without Vault key material to leave Vault in `sealed_unavailable`, so that the rest of the database can still come up.

## Implementation Decisions

- `CollectionModel` gains formal `Config` and `Vault` variants beside `Kv`.
- DDL uses distinct forms: `CREATE KV`, `CREATE CONFIG`, `CREATE VAULT`, plus model-aware `DROP` and typed `SHOW` filters.
- Normal KV remains the scope of PRD #238. TTL, INCR, DECR, ADD/merge, and destructive invalidation belong to normal KV only.
- Config supports PUT/GET/DELETE/ROTATE/HISTORY/PURGE, optional type/schema metadata, tombstone delete, and explicit SecretRef resolution.
- Vault supports PUT/GET/UNSEAL/DELETE/ROTATE/HISTORY/PURGE, redacted metadata reads, sealed storage, and mandatory audit.
- CAS uses `EXPECT VERSION` and `EXPECT NULL` as the shared MVP contract. Vault may additionally support `EXPECT FINGERPRINT`; `EXPECT VALUE` is out of scope for MVP.
- Vault encryption at rest is mandatory. Config and normal KV are plaintext in the MVP.
- `CREATE VAULT name` uses cluster master key material. `CREATE VAULT name WITH OWN MASTER KEY` derives per-vault key material.
- `red.config` and `red.vault` are bootstrap-created system collections. Users cannot create/drop/truncate them through normal DDL.
- Legacy pseudo-paths are compatibility aliases only. Policy and audit normalize to explicit targets such as `vault:red.vault/cluster.join_token`.
- Public APIs are separated by domain: `/v1/kv`, `/v1/config`, `/v1/vault`; MCP tools and drivers follow the same split.

## Testing Decisions

- Parser tests should prove valid DDL/operations for all three models and invalid model-operation combinations.
- Runtime tests should verify Config history/tombstone/purge, Vault redaction/unseal/audit, and normal KV TTL/counters staying KV-only.
- Transport tests should run equivalent scenarios over HTTP, MCP, pgwire/simple-query, and drivers where applicable.
- Security tests should assert that Vault watch/list/get never expose plaintext or ciphertext.
- Backup/restore tests should assert that Vault restores as sealed data and handles missing key material as `sealed_unavailable`.

## Out of Scope

- Optional encryption for normal KV or Config.
- Redis structures beyond normal keyed values.
- Automatic secret resolution from normal `GET CONFIG`.
- Per-tenant cryptographic isolation; per-vault key material is the MVP isolation step.
- `TRUNCATE VAULT`; Vault bulk destructive operations require future design.

## Further Notes

Terminology matters: do not use "KV Vault", "KV Secrets", or "KV Config". Use "KV", "Config", and "Vault" as separate keyed collection models.
