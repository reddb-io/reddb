# Changelog

All notable changes to this project are documented here. Versions follow
[SemVer](https://semver.org/) and the engine + locked-step npm/PyPI
packages share one number (see `scripts/check-versions.sh`).

## 1.0.0 — 2026-05-06

First stable release. Engine, drivers, and CLI move to the `@reddb-io/*`
npm org and lock at `1.0.0`.

### npm migration (#125)

- `@reddb-io/cli` — CLI launcher (was `reddb-cli`).
- `@reddb-io/sdk` — JS/TS driver (RedWire/gRPC/HTTP/embedded).
- `@reddb-io/client` — thin remote-only driver, downloads `red_client`
  binary post-install (#136, #137, #138).
- `scripts/sync-version.js` propagates engine bumps to all locked-step
  packages including `drivers/js-client/`.
- `scripts/check-versions.sh` enforces lock-step in CI.
- Legacy `reddb-cli` npm package deprecated post-publish via
  `scripts/deprecate-legacy-npm.sh` (operator-triggered, see
  `docs/release-runbook.md`).

### Engine

- UnifiedRecord storage layout (perf push closure).
- WAL lock-free append path.
- Pager striped lock to remove the global page-cache contention.
- BTree batch ingest path.
- AggregateQueryPlanner for push-down aggregates.
- IncrementalIndex maintenance.

### Topology (#164)

- Topology discovery PRD substantially shipped: advertiser, consumer, and
  router landed; E2E coverage in flight (#172).
- Strategic gate ADR 0009 — gate is scenario-specific, not global.

### Out of scope for this slice

- The unscoped `reddb` npm name is owned by an unrelated upstream package
  and is not deprecated.
- `drivers/python-asyncio` and `charts/reddb` follow independent version
  policies.
