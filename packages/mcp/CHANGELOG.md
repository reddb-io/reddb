# @reddb-io/mcp

## 1.26.3

## 1.26.2

## 1.26.1

## 1.26.0

## 1.25.7

## 1.25.6

## 1.25.5

## 1.25.4

## 1.25.3

## 1.25.2

## 1.25.1

## 1.25.0

## 1.24.15

## 1.24.14

## 1.24.13

## 1.24.12

## 1.24.11

## 1.24.10

## 1.24.9

## 1.24.8

## 1.24.7

## 1.24.6

## 1.24.5

## 1.24.4

## 1.24.3

## 1.24.2

## 1.24.1

## 1.24.0

## 1.23.4

## 1.23.3

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
