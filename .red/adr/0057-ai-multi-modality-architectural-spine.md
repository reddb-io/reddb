# ADR 0057 — AI multi-modality architectural spine

Status: accepted
Date: 2026-06-20

## Decision

reddb's AI surface becomes a **declarative, per-collection property of the data**:
configured once in DDL, enforced automatically on every write, across four
modalities (embed / generate / vision / moderate) and across SaaS and
locally-run providers. This ADR records the architectural spine of that work and,
prominently, the consciously-rejected options. The full feature set and its
rationale are decided in **PRD #1267**; this is a durable decision record, not a
re-decision.

Four load-bearing decisions form the spine.

### 1. Hybrid write-path coupling

AI does not couple to writes uniformly. The coupling is split by what each
modality *means*:

- **Moderation is a synchronous pre-commit gate.** Rejecting content after it has
  persisted is pointless, so moderation must run before commit and can fail the
  write.
- **Embedding and vision are asynchronous enrichments** carried over reddb's
  existing CDC stream. Eventual attachment is acceptable for them, and running
  them off the write path keeps normal write latency and availability independent
  of any AI provider.

The async half rides the existing change-data-capture machinery — the
`cdc_emit` / `emit_update_events_for_collection` LSN stream and the `ChangeEvent`
records in the `replication::cdc` surface — rather than a new queue. A CDC-driven
enrichment consumer computes embeddings/vision for committed rows, attaches the
results, and manages a `pending` status; rows whose enrichment has not completed
carry `pending` and are excluded from vector/vision queries until ready, so a
vector search never silently returns an incomplete set. Failures retry with
backoff and then dead-letter, with an ops re-drive path.

### 2. Moderation degraded mode

When the moderation provider is unavailable, the default is **fail-open +
quarantine**: the write commits, but the row is invisible to normal reads until
it is re-moderated over CDC. This keeps the database writable during a provider
outage while never serving unmoderated content. Per collection, this is
configurable to **hard fail-closed** (provider outage blocks the write) for
high-risk content.

A quarantined row that later **re-moderates to a reject** is **tombstoned and
permanently hidden** (soft-delete, retained for audit and an appeal/override
path) by default. Hard-delete on reject — the row leaves no trace at all — is a
per-collection stricter opt-in. Quarantined, pending, and tombstoned rows are all
excluded from normal reads under one consistent visibility rule.

### 3. Provider modality-capability matrix

Provider capability is modelled as a **matrix over modalities** (embed / generate
/ vision / moderate), built by generalizing the existing AI `provider_capabilities`
registry (the `Registry` / `Capabilities` types in the `runtime::ai` surface) with
a modality dimension. The registry already has the right shape — per-provider
rows, per-deployment overrides, and conservative defaults for unknown tokens.

A collection's AI policy is **validated at DDL time** against the matrix: a policy
that references a provider/model incapable of a requested modality is rejected
immediately, not on the first insert. Call-time gating is a second check.

### 4. Per-collection AI policy home

The AI policy lives in the **collection schema/catalog**, declared via
`CREATE/ALTER COLLECTION ... WITH (...)` options alongside the existing
`tenant_by` / `append_only` options, and is versioned and migrated with the
schema. The policy describes, per modality, which fields are embedded/moderated,
the moderation sync gate + degraded mode + reject action, and the vision
image-reference field and output kinds. It is read by the write path from the
catalog, so it is a single source of truth that travels with the collection.

Provider credentials resolve from the **encrypted vault** (the policy-scoped
vault of ADR 0027, holding KV records) with `REDDB_<PROVIDER>_API_KEY` env vars as
a zero-config bootstrap fallback.

## Rejected, and why

- **Fully synchronous, fail-closed coupling for all modalities.** Rejected. It
  makes *every* write hostage to the AI provider's latency and availability —
  including embedding and vision, where eventual attachment is perfectly
  acceptable. The hybrid split applies synchronous coupling only where the
  semantics demand it (moderation-as-gate) and keeps the rest off the write path.

- **Fully asynchronous coupling for all modalities.** Rejected for moderation:
  moderation-as-block cannot work once the content has already landed and been
  served. Async is correct for embedding/vision, not for the pre-commit gate.

- **Pure fail-closed as the default degraded mode.** Rejected as the default — a
  provider outage would become a write outage. Offered as a per-collection opt-in
  for high-risk content instead. (The mirror option, pure permissive fail-open,
  is also rejected: it serves unmoderated content during the outage. Quarantine
  is the availability-preserving middle.)

- **Hard-delete as the default reject action for a re-moderated quarantined row.**
  Rejected as default — it produces a surprising "insert succeeded, then the row
  vanished" and loses the audit trail. Default is tombstone + hide; hard-delete is
  a per-collection stricter opt-in.

- **In-process candle inference now.** Rejected for this initiative. "Run models
  locally" is satisfied by external local runtimes (Ollama / OpenAI-compatible
  local servers), which already run HF/GGUF weights and cover essentially every
  real deployment. Wiring candle as an in-process ML runtime is a heavy separate
  project that would dominate this work; it stays a future option, not a denial.

- **A native binary/blob type for images.** Rejected for now. Vision input is a
  **URL/URI reference** stored in a row field; the pipeline fetches the image and
  reddb stores the reference, not the bytes. This keeps reddb a JSON document
  store with no new blob subsystem and no row/WAL/page bloat. (Inline base64 in a
  String field is likewise rejected — it bloats documents and the WAL with binary.)

- **A separate AI-policy config object or global server config.** Rejected — both
  create a second source of truth for a concern intrinsic to the collection. The
  policy belongs in the catalog with the schema (decision 4).

- **Separate per-modality provider traits/registries, or runtime-probe-only
  capability.** Rejected — separate registries fragment a unified concern, and
  runtime-probe-only defers failure to the most expensive moment (mid-write). The
  single matrix with DDL-time validation gives a fast declarative failure
  (decision 3).

## Consequences

- Write latency and availability stay independent of embedding/vision providers;
  only moderation (by design) can gate or quarantine a write.
- The async modalities reuse existing CDC machinery rather than introducing a new
  enrichment transport, keeping the write path and the stream as the two coupling
  points.
- Operators get one consistent visibility rule (pending / quarantined /
  tombstoned all hidden from normal reads) and a re-drive path for dead-lettered
  enrichments.
- The rejected options survive as recorded trade-offs: a future contributor sees
  that synchronous-everywhere, in-process candle, and a native blob type were
  deliberate non-choices, not oversights.

## Related

- PRD #1267 — AI multi-modality (decides the feature set and rationale this ADR records)
- ADR 0019 — RID and multi-model update surface (the row/update surface enrichment attaches to)
- ADR 0027 — Policy-scoped vault and config namespaces (provider-credential home)
- The `replication::cdc` surface — `ChangeEvent`, `cdc_emit`, `emit_update_events_for_collection` (async-enrichment transport)
- The `runtime::ai::provider_capabilities` registry — `Registry` / `Capabilities` (extended with the modality dimension)
- Issue #1268
