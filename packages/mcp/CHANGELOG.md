# @reddb-io/mcp

## 1.23.2

## 1.23.1

## 1.23.0

## 1.22.0

## 1.21.0

## 1.20.0

### Minor Changes

- Add `$kv.X` SQL syntax for plain user KV store

  Introduces `SET KV <key> = <value>`, `DELETE KV <key>`, and `$kv.<path>` inline references that desugar to `__KV_REF("red.kv/<path>")`. Access is gated by `kv:read` / `kv:write` IAM policies. Protects `red.secret.*` namespace from `$secret.X` resolution regardless of IAM role.

## 1.18.0

## 1.17.0

### Minor Changes

- [#1517](https://github.com/reddb-io/reddb/pull/1517) [`f1f286d`](https://github.com/reddb-io/reddb/commit/f1f286ddfa034ad07826cd1bf75ab8941c21b2ac) Thanks [@filipeforattini](https://github.com/filipeforattini)! - Harden release provenance with aggregate checksum manifests, GitHub Artifact
  Attestations, GHCR provenance/SBOM attestations, expanded release verification
  notes, and public support/release policy docs.

## 1.16.0
