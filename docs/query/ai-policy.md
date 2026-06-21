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
| `VISION (...)` | Image understanding from a reference field | Parses today; enforcement **planned** |

The architectural rationale (hybrid write-path coupling, moderation quarantine,
the provider modality matrix) is recorded in **ADR 0057**.

> [!IMPORTANT]
> Only the `EMBED` clause is wired end-to-end today (auto-embed over CDC). The
> `MODERATE` and `VISION` clauses **parse, validate, and persist** in the
> collection contract, but the moderation gate (content-moderation, in progress)
> and vision detections (computer vision, in progress) are not yet enforced.
> Declaring them is forward-compatible, not active.

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

## VISION — image understanding (planned)

> [!WARNING]
> The `VISION` clause parses, validates against the provider matrix, and
> persists today, but vision detections are **in progress and not yet active**.
> No vision output is attached to rows yet, and there is **no query predicate
> for filtering on vision output**. Declaring `VISION` is forward-compatible
> only.

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
| `provider` | Yes | Must support the `vision` modality |
| `model` | Yes | Vision-capable model name |

Like embedding, vision is designed as an **asynchronous enrichment** over CDC.
The image is referenced by URL; reddb does not introduce a binary/blob type for
image bytes.

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
