# Release Notes — 2026-04-23 → 2026-04-26

Window: last 72 hours of work on `main` plus uncommitted changes
on the working tree. Grouped by subsystem, ordered roughly by
user visibility.

## RedWire wire protocol

ADR 0001 (RedWire as evolution of v1) and ADR 0002 (phased rollout)
are now mostly executed. All four phases of ADR 0002 landed:

- **Phase 1 — per-frame zstd compression.** Optional, advertised in
  the v2 `Hello` capabilities, applied per-frame. Cuts bulk insert
  egress on slow links without changing v1 behaviour.
- **Phase 2 — TLS / mTLS dispatch + prepared statements + streaming
  bulk.** TLS-wrapped wire shares port 5050 with plaintext via the
  `0xFE` magic byte. Prepared statements and `BULK_STREAM_*` frames
  now flow through v2.
- **Phase 3 — SCRAM-SHA-256 over the v2 handshake.** Server primitives
  in `src/auth/scram.rs`; client primitives in
  `drivers/rust/src/redwire/scram.rs`. Stored credential format:
  `SCRAM-SHA-256$<iter>:<salt>:<stored-key>:<server-key>`. RFC 5802
  state machine identical to PG-wire SASL.
- **Phase 4 — OAuth / OIDC JWT over the v2 handshake.** Same
  `JwtVerifier` validator already used by HTTP and gRPC; `iss`,
  `aud`, `exp`, `nbf` enforced, `preferred_username` claim mapped
  to RedDB identity by default.

Steady-state perf is unchanged: v2 hands off to the v1 binary
handlers after the handshake, so per-query overhead is the
extra 11 bytes in the frame header. Spec freezes after this
phase block; subsequent feature work bumps `Hello.versions[]`.

## Drivers

### JavaScript / TypeScript (`drivers/js`)

- Unified `red://` connection-string parser landed. The driver
  picks the right transport from one URL.
- HTTP / HTTPS adapter (`drivers/js/src/http.js`).
- Native RedWire v2 TCP transport (`drivers/js/src/redwire.js`)
  with bridge to v1 binary fast path (`MSG_BULK_INSERT_BINARY`,
  `MSG_QUERY_BINARY`, `MSG_PREPARE`/`EXECUTE_PREPARED`,
  `BULK_STREAM_*`).
- mTLS through `reds://` scheme + `tls: { ca, cert, key }` options.
- 6-transport matrix: embedded, HTTP, HTTPS, RedWire-TCP,
  RedWire-TLS, RedWire-mTLS. Plus PG-wire fallback via the
  upstream `pg` driver.
- `red://:memory` and `red://:memory:` aliases land as embedded
  in-memory shorthands (SQLite-style).
- `Insert`, `BulkInsert`, `Get`, `Delete` frames close the basic
  CRUD parity with the engine.
- Standalone `login()` helper for username/password → bearer flow.
- TS types in `drivers/js/index.d.ts` updated for every transport.

### Rust (`drivers/rust`)

- HTTP / HTTPS transport (`drivers/rust/src/http.rs`).
- TLS + mTLS for the RedWire client (`drivers/rust/src/redwire/tls.rs`)
  reaching parity with the JS driver.
- SCRAM-SHA-256 client primitives + advertised methods
  (`drivers/rust/src/redwire/scram.rs`).
- Embedded driver adapted to the engine's `Arc<str>` schema-key
  types (no more clones at the edge).
- `redwire_smoke.rs` integration test exercises the full handshake
  (compression + TLS + SCRAM + OAuth + bulk + streaming).

### Cross-language release flow

`pnpm`-driven cross-language version sync (mirrors the redblue
release flow). One `pnpm release:bump <semver>` updates
`Cargo.toml`, `drivers/rust/Cargo.toml`, `drivers/python/Cargo.toml`,
`drivers/python/pyproject.toml`, `drivers/js/package.json`, and the
root `package.json` together.

## Authentication

- **SCRAM-SHA-256 end-to-end** — RedWire v2 + PG wire + user vault
  storage format documented in [`security/tokens.md`](security/tokens.md#scram-sha-256).
- **OAuth / OIDC JWT** — `AuthConfig.oauth` validator now serves
  HTTP, gRPC, and RedWire v2 from the same code path.
- **HMAC-signed requests** — new scheme with timestamp + nonce
  replay protection; canonical string is
  `{method}\n{path}\n{timestamp}\n{nonce}\n{sha256(body)}`.
  Headers: `X-RedDB-Key-Id`, `X-RedDB-Timestamp`, `X-RedDB-Nonce`,
  `X-RedDB-Signature`.
- **`_FILE` secrets convention** — every sensitive env var
  (`RED_ADMIN_TOKEN`, `RED_S3_SECRET_KEY`, `RED_BACKEND_HTTP_AUTH`,
  `RED_TURSO_TOKEN`, `RED_D1_TOKEN`, …) honours an `*_FILE`
  companion that wins over the inline value.
- **Live secret rotation via SIGHUP** — sending SIGHUP reloads
  every `*_FILE` companion in place. No more pod rolls for token
  rotation.

## Replication & Commit Policy

- **`CommitPolicy` enum** (`src/replication/commit_policy.rs`):
  `Local | RemoteWal | AckN | Quorum`. Set via
  `RED_PRIMARY_COMMIT_POLICY` (or per-request in bulk RPCs).
  Default `local`.
- **`CommitWaiter`** primitive — the writer surface waits on
  per-replica durable LSN before acking the client. On deadline
  expiry the request returns `commit_wait_timed_out`; the
  metric `reddb_commit_wait_total{outcome="timed_out"}` increments.
- **`AckReplicaLsn` gRPC** — replicas durably-ack their applied
  LSN to the primary. Per-replica state (`last_seen`, `last_sent`,
  `last_durable`) is now visible in `/admin/replicas`.
- **`LogicalChangeApplier`** stateful applier — surfaces typed
  errors `Gap`, `Divergence`, `Apply`, `Decode`. Replicas in
  `divergence` refuse promotion.
- **HTTP + gRPC commit-policy enforcement** in DML, bulk, and
  graph paths so the policy is honoured no matter which surface
  served the write.

## Backends & Writer Lease

- **Conditional-write trait surface** — every backend implementing
  CAS/version-tokens unlocks the writer lease.
  - Local FS: content-hash CAS + exclusive flock.
  - S3-compatible: ETag + `If-Match` on PUT/DELETE.
  - Generic HTTP: opt-in via `RED_HTTP_CONDITIONAL_WRITES=true`.
  - Turso / D1: single-writer by construction.
- **`RemoteBackend` → `AtomicRemoteBackend` split** — non-CAS
  paths are isolated in the original trait so misuse is a compile
  error rather than a runtime data-loss surprise.
- **`RED_LEASE_REQUIRED=true`** — fail-closed boot when the chosen
  backend cannot enforce conditional writes. The CLI lease loop
  refreshes the lease on a heartbeat; loss → the runtime rejects
  every write boundary with `lease_not_held`.
- **Lease lifecycle** centralised in `LeaseLifecycle` (
  `src/runtime/lease.rs`). Acquire/refresh/release are CAS-based.
- **Writer-lease state on `/admin/status`** + metric
  `reddb_writer_lease_state{state="not_required|held|not_held"}`.

## Serverless & Cloud

- **Auto-restore from remote on cold boot** when `RED_AUTO_RESTORE=true`.
- **Cloud-agnostic backend selection** via `RED_BACKEND` (`s3`,
  `fs`, `http`, `turso`, `d1`, `none`).
- **Lifecycle contract** — `/admin/shutdown`, health probes,
  signal handlers (SIGTERM drain + final backup, SIGHUP secret
  reload, SIGUSR1 checkpoint).
- **Hot-path quota enforcement** — `RED_MAX_QPS_PER_CALLER` token
  bucket keyed by `bearer:<sha256-prefix>` / `replica:<id>` /
  `anon`, surfaced in `reddb_quota_rejected_total{principal=…}`.
- **`ResourceLimits`** from `RED_MAX_*` env vars, surfaced both
  in `/metrics` (`reddb_limit_*`) and `/admin/status`.
- **Dynamic read-only toggle** via `/admin/readonly`.
- **`/metrics` Prometheus** + **`/admin/status` JSON** snapshots.
- **Generic `HttpBackend`** + admin restore/backup endpoints.
- **`ServerSurface` enum** (`Public | AdminOnly | MetricsOnly`)
  — operators can pin admin and metrics to dedicated listeners.
- **Reference deployment manifests** for AWS ECS Fargate, AWS App
  Runner, AWS Lambda+EFS (read replica), Azure Container Apps,
  Cloudflare Containers, Fly Machines, Google Cloud Run, HashiCorp
  Nomad, and Kubernetes (StatefulSet + PVC).
- **`Dockerfile.musl`** — static-binary container image
  (`release-static` profile, `panic = "abort"`), suitable for
  distroless / scratch base images.
- **systemd unit** + **Dockerfile health probe** wired through the
  same `red doctor` exit-code contract.

## Safety

- **Logical WAL crash safety** — CRC32 + `sync_all` + valid-prefix
  recovery. Restore validates the WAL hash chain end-to-end; a
  break aborts restore with a typed `chain` error.
- **WAL segment hash chain via `prev_hash`** — manifest now exposes
  the chain so external verifiers can audit it without parsing
  individual segments.
- **WAL segment SHA-256** + per-artifact sidecars + unified
  `MANIFEST.json` — manifest swap is atomic via conditional write.
- **Snapshot SHA-256 integrity check on PITR restore.**
- **Read-only enforcement at every public mutation boundary** —
  HTTP, gRPC, native wire, and the admin API all check the gate
  before entering the storage path.
- **`WriterLease` primitive** for serverless writer fencing.
- **Panic policy** — release builds use `panic = "abort"`. RedDB
  treats unexpected panic as process-fatal because unwinding through
  write/recovery/replication paths can leave in-memory state
  inconsistent with the WAL. Documented in
  [Operator Runbook §6](operations/runbook.md#panic-policy).

## Observability

- **`reddb_cold_start_phase_seconds{phase}`** — per-phase breakdown
  (`restore`, `wal_replay`, `index_warmup`, `total`). Phase markers
  written by `LifecycleSnapshot::set_*`.
- **`reddb_slo_lag_budget_remaining_seconds{replica_id}`** —
  `RED_SLO_REPLICA_LAG_BUDGET_SECONDS` minus measured lag; negative
  means the SLO budget is exhausted.
- **`reddb_replica_apply_health{state}`** — per-state gauge so
  dashboards can pivot on `ok|connecting|stalled_gap|divergence|apply_error`.
- **`reddb_primary_commit_policy{policy}`** + `reddb_commit_wait_*`
  counters.
- **`reddb_quota_rejected_total{principal}`** for per-caller
  throttling.
- **OpenTelemetry config scaffold** behind `--features otel`
  (`src/observability/otel.rs`).

## Architecture refactors

- **Cluster 3 — `RemoteBackend` split.** Non-CAS APIs moved out of
  the atomic trait; `AtomicRemoteBackend` is the only path that
  can serve the writer lease.
- **Cluster 4 — pager owns page-level encryption.** `EncryptedPager`
  is deprecated in favour of pager-internal encryption hooks.
- **Cluster 5 — `run_use_case` dispatch.** Server handlers no
  longer hand-roll runtime → use-case glue; everything goes
  through the centralised dispatcher.
- **Cluster 6 — `OperationContext` + `WriteConsent`.** Five
  mutating ports now carry an explicit context object that carries
  caller identity, write consent, and trace metadata. Gates the
  writer surface from HTTP, gRPC, RedWire v2, and PG wire alike.
- **`service_router` split** into `ProtocolDetector` + `Router`.
- **Lease state machine** centralised in `LeaseLifecycle`.

## CLI

- **`red doctor`** — probes `/metrics` + `/admin/status` against
  operator-tunable thresholds, exits `0|1|2`. Designed for CI
  gates, on-call playbooks, and Kubernetes liveness wrappers.
  Detail: [`api/cli.md#red-doctor`](api/cli.md#red-doctor).

## CI / Release

- **Chaos + drill jobs** — chaos-minio, drill-nightly with
  issue-on-failure.
- **Feature-matrix CI** — every published feature combination is
  built independently per PLAN B5.
- **Cold-start P95 driver** + **artifact-sizes gate**.
- **Crates.io publish dry-run gate** + tightened packaging.
- **Mirrors of the redblue release flow** — runbook in `RELEASING.md`.

## Performance (older, included for context)

The following landed slightly before the 72h window but are part of
the same release cycle:

- Composite B-tree sorted index for `AND(Eq, Range)`.
- Zero-copy direct scan for unfiltered `SELECT * LIMIT N`.
- Plan-cache reuse for `UPDATE` / `DELETE` via shape normalisation.
- Single-pass literal binds (fused normalize + extract) in the plan cache.
- `Arc<str>` for `CachedPlan.exact_query` (skip clone on hot path).
- `Arc`-share `CollectionContract` to skip clones on UPDATE.
- Skip in-line B-tree upsert on the PG-HOT-style UPDATE path.
- Persist entity refs instead of cloning on UPDATE.
- Indexed-columns set skips `RegisteredIndex` clone on UPDATE.
- Push `LIMIT` down into unfiltered `SELECT *` scan.
- Parallel single-column `COUNT(*) GROUP BY` (Phase 2C).
- Prepared statements + cursors, bounded bulk stream, top-k quickselect.

## See also

- [ADR 0001 — RedWire TCP Protocol](adr/0001-redwire-tcp-protocol.md)
- [ADR 0002 — RedWire v2 rollout](adr/0002-redwire-v2-rollout.md)
- [Auth & Security Overview](security/overview.md)
- [Auth Methods, Tokens & Keys](security/tokens.md)
- [Connection Strings](clients/connection-strings.md)
- [SDK Compatibility](clients/sdk-compatibility.md)
- [Replication](deployment/replication.md)
- [Backends](deployment/backends.md)
- [Serverless Mode](deployment/serverless.md)
- [Operator Runbook](operations/runbook.md)
- [Metrics Spec](spec/metrics.md)
