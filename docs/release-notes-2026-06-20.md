# Release Notes — 2026-05-30 → 2026-06-20

Window: roughly three weeks of work on `main`. Grouped by subsystem,
ordered by user visibility. Two large initiatives landed in this
window — the **AI multi-modality spine** (PRD #1267, ADR 0057) and the
**TigerStyle house style + workspace crate split** (PRD #1252, ADRs
0052–0056) — alongside operational-storage and clustering design work
that is mostly captured as accepted/proposed ADRs rather than shipped
runtime behaviour.

> Where a feature is partly landed, this note says so explicitly.
> "Shipped" means it is live on `main`; "planned" / "proposed" means the
> contract or scaffolding exists but the runtime behaviour is not yet
> wired.

## AI & multi-modality

The AI subsystem moved from "embeddings + one-shot prompts" toward a
configurable, per-collection, multi-modal model surface (ADR 0057).

- **`ASK … AS RQL` with an inference backend (shipped).** `ASK` can now
  translate a natural-language question into RQL through the configured
  model. Two backends: `deterministic` (the default, template-based) and
  `llm` (inference). Either way the produced query is **always parsed and
  validated** before it is returned. The default is *candidate-only* — it
  hands back the RQL without running it.
- **`EXECUTE` opt-in (shipped).** Append `EXECUTE` to run the validated
  candidate. Execution is **read-only**: a candidate that parses to a
  mutating statement is refused, not run. MCP- and gRPC-built `ASK`
  queries default to `execute=false`.
- **Per-collection AI policy via DDL `WITH (…)` (shipped for `EMBED`).**
  `CREATE TABLE … WITH (EMBED …)` attaches an embedding policy to a
  collection. The policy is validated at DDL time against the provider
  modality matrix and persisted in the catalog (introspectable). See the
  new [AI Policy (per-collection)](/query/ai-policy.md) page.
- **Auto-embed over CDC (shipped).** With an `EMBED` policy, inserts and
  updates are embedded **asynchronously after commit** by the CDC
  enrichment consumer. Rows are **excluded from `VECTOR SEARCH` until the
  embedding is attached** (`pending`); enrichment failures retry, then
  move to a dead-letter state with re-drive. The enrichment path
  currently drives the `local` embedding backend.
- **Provider modality matrix + MiniMax (shipped).** Providers now carry a
  modality dimension — `embed`, `generate`, `vision`, `moderate` — and a
  built-in capability table gates which provider can serve which
  modality, with DDL-time validation. **MiniMax** was added as a
  provider. See [AI Providers](/guides/ai-providers.md) and
  [AI Provider Modes](/api/ai-provider-modes.md).
- **Vault-backed provider credentials (shipped).** Provider API keys
  resolve from the encrypted vault first, falling back to environment
  variables.
- **Computer-vision detections over CDC (shipped — local backend, #1275).**
  A `VISION` policy fetches the referenced image (`http(s)`/`file`/bare
  path), analyzes it, and attaches a structured detections array to the
  derived `vision_detections` field, with the same retry/dead-letter
  behaviour as `EMBED`. **Caveat:** analysis runs the in-process `local`
  backend only — a remote `provider` (e.g. `openai`) validates at DDL time
  but is rejected at enrichment time, so end-to-end against a hosted vision
  model is not yet wired.
- **`CONTAINS` descends into JSON objects/arrays (shipped, #1275).** The
  live query path now supports `WHERE CONTAINS(vision_detections, 'person')`
  and JSON-object containment generally.
- **Content moderation gate (planned, #1274).** The `MODERATE` policy clause
  **parses, validates, and persists**, but the write-path moderation gate
  is still in progress and **not active**. Do not rely on it yet.

## House style & correctness (TigerStyle)

Adapted from TigerBeetle's TIGER_STYLE, codified in `STYLE.md` and ADR
0056 (PRD #1252). Developer-facing; see the new
[House Style](/dev/house-style.md) page.

- **`[workspace.lints]` scaffolding** with an `unwrap → expect` ratchet,
  so new `unwrap()`s in production paths are caught.
- **`clippy::too_many_lines`** as a warn-as-ratchet function-length nudge.
- **Truncating-cast lints** (`cast_possible_truncation`,
  `as_conversions`).
- **Storage hotspots migrated** (`btree`/`pager`/`wal`) from `unwrap` to
  `expect`/`Result`.
- **RQL depth-cap generalised** (`JSON_LITERAL_MAX_DEPTH`) across
  expression and subquery nesting, with a bounded parser-fuzz target in
  CI.

## Workspace & crate authority

The workspace was split into authority crates with explicit boundaries
(ADRs 0046, 0052, 0053, 0054). See
[Monorepo Structure](/dev/monorepo-structure.md).

- **`reddb-io-types`** — the keystone vocabulary crate, below the server.
- **`reddb-io-rql`** — the SQL/RQL front end (lexer, AST, parser,
  planner, optimizer); executors stay in the server.
- **`reddb-io-crypto`** — the page-envelope crypto authority (ADR 0054):
  a magic-less AES-256-GCM envelope, **28 bytes of overhead** (12-byte
  nonce + 16-byte tag), page-id as AAD. This is **dormant** at runtime —
  it is distinct from the active vault encryption used for secrets. See
  [Encryption at Rest](/engine/encryption.md).
- **`reddb-io-wire` / `reddb-io-file`** — the wire-vs-file codec
  authority boundary (ADR 0046).

> ADR numbering note: there are two ADRs numbered 0052
> (`reddb-io-types-keystone` and `cluster-supervisor-control-plane-consensus`).
> A renumber is pending; both are referenced by name in the docs.

## Clustering & operational storage (mostly design)

Most of this window's clustering and operational-storage work landed as
**accepted or proposed ADRs**, not as runtime behaviour. Treat it as
design intent.

- **Fixed hash-slot primitive (shipped, inert).** A 16,384-slot hash-slot
  layer (`cluster/slot.rs`, BLAKE3 → slot, slots folded into the range
  catalog) landed (#1218). It is consumed by the control-plane topology
  models but is **not wired into request serving**, and there is **no
  `SHARD BY` DDL**.
- **Supervisor control-plane consensus boundary accepted (ADR 0052).**
  The boundary is decided; the durable replicated control-plane log
  itself is a named follow-up slice, not built.
- **Operational storage profiles / collection layouts / manifest &
  DDL recovery / backup-restore boundary / cluster range file layout
  (ADRs 0038–0045, 0055 — proposed).** Captured on the new
  [Operational Storage Profiles](/engine/operational-storage-profiles.md)
  reference page as a target design.
- **Container topology helm modes (shipped, #1204).**
  `standalone` / `serverless` / `primary-replica` / `cluster` modes in
  the Helm chart. See [Kubernetes and Helm](/deployment/kubernetes.md).

## Auth, bootstrap & operations

- **First-boot auth presets (shipped, #1236).** `REDDB_BOOTSTRAP_PRESET`
  selects a first-boot posture — `simple` (default), `production`,
  `regulated`, `cloud` — each with its own users/policy/auth defaults.
  See [First Boot](/deployment/first-boot.md).
- **Policy-first bootstrap guardrails (shipped, #1224).** Bootstrap is
  now policy-first: `system_owned` mutations are rejected, managed
  policies gate `user:*` actions.
- **Per-collection `on_disk_bytes` telemetry (shipped, #1240).** Surfaced
  via the `red.collections` virtual table (queried with SQL), not via
  `/stats` or Prometheus. See [Health & Observability](/reference/health.md).
- **Discovered HTTP route catalog (shipped, #1251).**

## Storage engine

- **Columnar read decode (shipped, #962).** Typed zero-copy column-batch
  and row decode for the columnar (RDCC) format. It is used by the
  time-series / hypertable read bridge; it is **not yet wired into
  general `SELECT` execution**, which still uses the row engine. Bench
  detail in [the columnar-read note](/perf/2026-06-03-columnar-read.md).
- **Embedded snapshot fix (shipped, #1186).** Avoid an embedded snapshot
  on paged reopen.

## CI & dependencies

- Hardened chaos backends (minio/s3 via floci), restored the nightly DR
  drill target, grouped runtime/persistence/AI integration test targets,
  installed `protoc` for the parser-fuzz nightly, and wired coverage
  gates and changeset checks.
- Routine dependency bumps (prost, http, regex, insta, webpki-roots,
  alpine, GitHub Actions group).
