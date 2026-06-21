# Per-collection AI policy

A collection can declare an **AI policy** in its DDL. The policy makes AI a
declarative property of the data: configured once on the collection, persisted
in the catalog, and read by the runtime — rather than something every write has
to re-specify.

The policy is split into one clause per modality, all inside the `CREATE TABLE
... WITH (...)` option list, next to the existing `tenant_by` / `append_only`
options:

| Clause | Modality | Status |
|:-------|:---------|:-------|
| `EMBED (...)` | Auto-embed declared fields over CDC | **Available** |
| `MODERATE (...)` | Pre-commit content moderation gate | Parses today; enforcement **planned** |
| `VISION (...)` | Image detections from a reference field | **Pipeline shipped** (local backend; remote providers pending) |

The architectural rationale (hybrid write-path coupling, moderation quarantine,
the provider modality matrix) is recorded in **ADR 0057**.

> [!IMPORTANT]
> `EMBED` and `VISION` run over CDC today. `EMBED` is wired end-to-end;
> `VISION` ships its full pipeline but only against the in-process `local`
> backend (remote vision providers validate at DDL time but are rejected at
> enrichment time — see below). The `MODERATE` clause **parses, validates, and
> persists** in the collection contract, but the moderation gate is **in
> progress and not yet enforced** — declaring it is forward-compatible, not
> active.

---

## EMBED — auto-embed over CDC

`EMBED` declares which text fields are automatically vectorised after every
write. The write path itself does no provider work: an `INSERT`/`UPDATE` commits
and returns immediately, and a [CDC enrichment consumer](#how-auto-embed-works)
computes and attaches the vector asynchronously.

```sql
CREATE TABLE articles (id INT, title TEXT, body TEXT)
WITH (
  EMBED (fields = ('title', 'body'), provider = 'openai', model = 'text-embedding-3-small')
)
```

### Options

| Option | Required | Description |
|:-------|:---------|:------------|
| `fields` | Yes | One or more source columns whose text is embedded, e.g. `('title', 'body')` |
| `provider` | Yes | Provider token (`openai`, `minimax`, `local`, …) — must support the `embed` modality |
| `model` | Yes | Embedding model name as the provider expects it |

All three options are required; omitting any of them is a DDL error.

### How auto-embed works

1. An `INSERT`/`UPDATE` on an `EMBED` collection commits normally and emits its
   usual CDC change event. **Write latency stays independent of the AI
   provider.**
2. A CDC enrichment consumer drains the LSN-ordered change stream, joins the
   declared `fields` into one text value, embeds it through the policy's
   provider, and attaches the result as a vector in the same collection —
   exactly as a manual `INSERT ... WITH AUTO EMBED` would.
3. Until the vector is attached, the row is **`pending`**: it is naturally
   excluded from `VECTOR SEARCH` (no vector exists yet), so a vector search
   never returns a half-enriched set.

Inserts always enrich. An update re-enriches only when it touched one of the
declared `fields`. Deletes do not enrich.

### Retry, dead-letter, and re-drive

| State | Meaning |
|:------|:--------|
| `pending` | Enrichment queued or in progress; row not yet vector-searchable |
| retry | A provider failure re-schedules the row with exponential backoff |
| dead-letter | After the attempt budget is exhausted, the row is parked for operators |

The consumer retries a failed embedding with exponential backoff (base × 2^n).
After the configured number of attempts (default 3) the work item is
**dead-lettered** and surfaced to operators with its last error. Operators can
**re-drive** dead-letters back into the pending set with a fresh attempt budget
— for example after fixing a provider credential or outage.

> [!NOTE]
> The end-to-end enrichment path currently drives the in-process `local`
> embedding backend. A collection whose `EMBED` policy names another provider
> parses and persists, and the enrichment consumer treats an unsupported
> provider as a retryable failure (it will retry then dead-letter).

---

## MODERATE — content moderation gate (planned)

> [!WARNING]
> The `MODERATE` clause parses, validates against the provider matrix, and
> persists in the collection contract today, but the moderation enforcement
> pipeline is **in progress and not yet active**. Declaring it does not yet
> screen, quarantine, or reject writes. The grammar below is stable; treat the
> behaviour as a preview.

```sql
CREATE TABLE comments (id INT, body TEXT)
WITH (
  MODERATE (
    fields  = ('body'),
    provider = 'openai',
    model    = 'omni-moderation-latest',
    sync     = true,
    degraded = closed,
    on_reject = flag
  )
)
```

### Options

| Option | Required | Values | Default | Description |
|:-------|:---------|:-------|:--------|:------------|
| `fields` | Yes | column list | — | Source fields screened by the moderation provider |
| `provider` | Yes | provider token | — | Must support the `moderate` modality |
| `model` | Yes | model name | — | Moderation model |
| `sync` | No | `true` / `false` | `false` | When `true`, moderation is a synchronous pre-commit gate |
| `degraded` | No | `open` / `closed` | `open` | Behaviour when the provider is unavailable — `open` lets the write through, `closed` rejects it |
| `on_reject` | No | `reject` / `flag` / `redact` | `reject` | What happens to content that fails moderation |

The intended design (per ADR 0057) couples moderation **synchronously** to the
write — rejecting content after it has persisted is pointless. The architectural
record also describes a fail-open + quarantine degraded posture and a
tombstone-on-reject visibility rule; those are decided in the ADR and land with
the moderation pipeline, not with this DDL surface.

---

## VISION — image understanding

> [!NOTE]
> The vision **pipeline shipped** in #1275: with a `VISION` policy, the CDC
> enrichment consumer fetches the referenced image (`http(s)://`, `file://`,
> or a bare path), analyzes it, and attaches a structured detections array to
> the derived `vision_detections` field — which is filterable from RQL with
> `CONTAINS` (see below). Retry / dead-letter behaves like the `EMBED` path.
>
> **Caveat — local backend only.** The analysis currently runs an in-process
> deterministic backend (the `local` provider). A remote `provider` such as
> `openai` **validates at `CREATE TABLE` time** (it supports the `vision`
> modality) but is **rejected at enrichment time** — only `local` actually
> runs today. End-to-end vision against a hosted model is not yet wired.

```sql
CREATE TABLE photos (id INT, image_url TEXT)
WITH (
  VISION (
    image_field = 'image_url',
    outputs     = ('caption', 'tags'),
    provider    = 'openai',
    model       = 'gpt-4o'
  )
)
```

### Options

| Option | Required | Description |
|:-------|:---------|:------------|
| `image_field` | Yes | Column holding the image **reference** (a URL/URI — reddb stores the reference, not the image bytes) |
| `outputs` | Yes | Output kinds to request, e.g. `('caption', 'tags', 'objects')` |
| `provider` | Yes | Must support the `vision` modality (validated at DDL time; only `local` runs at enrichment time today) |
| `model` | Yes | Vision-capable model name |

Like embedding, vision is an **asynchronous enrichment** over CDC. The image is
referenced by URL/URI; reddb does not introduce a binary/blob type for image
bytes. Detections land in the `vision_detections` field as
`[{label, confidence, bbox:[x,y,w,h]}]`; an optional image-embedding output
reuses the vector pipeline for image similarity.

### Filtering on detections

`CONTAINS` descends into JSON object/array values in the live query path
(#1275), so you can filter rows by detected label:

```sql
SELECT * FROM photos WHERE CONTAINS(vision_detections, 'person')
```

---

## DDL-time validation

Every AI clause is validated **at `CREATE TABLE` time** against the
[provider modality matrix](../api/ai-provider-modes.md#modality-matrix): a policy
that wires a provider to a modality it cannot serve is rejected immediately, not
on the first insert. For example, the `local` backend supports `embed` but not
`generate`/`vision`/`moderate`, so:

```sql
-- rejected at DDL time: 'local' cannot serve the moderate modality
CREATE TABLE t (id INT, body TEXT)
WITH (MODERATE (fields = ('body'), provider = 'local', model = 'x'));
```

A policy that references an **unknown provider token** is treated
conservatively: the matrix assumes only `embed`/`generate` for unknown tokens
and rejects `vision`/`moderate` requests against them.

---

## Introspection

The persisted AI policy travels with the collection contract and survives
restarts. Introspecting the collection shows the declared policy as part of its
schema, so the catalog is the single source of truth for "what AI runs on this
collection".

---

## See also

- [CREATE TABLE](create-table.md) — full table DDL and `WITH (...)` options
- [Vectors & embeddings](../data-models/vectors.md#auto-embed-over-cdc) — `AUTO EMBED` and the async enrichment model
- [AI providers](../guides/ai-providers.md) — provider tokens, the modality matrix, and vault credentials
- [AI provider modes](../api/ai-provider-modes.md#modality-matrix) — the modality dimension
- ADR 0057 — AI multi-modality architectural spine
