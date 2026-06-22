# Migrating from Snowplow

This guide is for teams running a Snowplow tracker today and pointing it
at RedDB instead of (or alongside) the Snowplow collector. RedDB does
not ship a drop-in collector binary — the migration path chosen in PRD
[#575](https://github.com/reddb-io/reddb/issues/575) is **a small
adapter you run between the tracker and RedDB's batch insert endpoint**.

The shape of the adapter is the same regardless of which tracker SDK
you use (JavaScript, iOS, Android, server-side). It translates the
Snowplow `payload_data` array into rows on a single RedDB collection —
`events` by convention — and POSTs them in batches to
`/collections/events/bulk/rows` (the batch insert endpoint).

A runnable Node implementation lives at
[`docs/examples/snowplow-adapter.mjs`](./examples/snowplow-adapter.mjs)
— roughly 50 lines, and CI runs it end-to-end against a real RedDB
instance so it can't silently break.

## Tracker payload → batch request

The Snowplow tracker POSTs a JSON envelope of the form:

```json
{ "payload_data": [ { "eid": "...", "stm": "...", "e": "ue", "ue_pr": "..." }, ... ] }
```

Map one tracker entry to one RedDB row using these five rules:

| Snowplow field          | RedDB row field          | Notes                                                                 |
|:------------------------|:-------------------------|:----------------------------------------------------------------------|
| `eid` (event_id, UUID)  | `event_id` (PK)          | Stable id for de-duplication on the server side.                      |
| `stm` / `dtm`           | `collector_tstamp`       | Epoch ms. Use whichever the tracker provided; default to `Date.now()`.|
| `ue_pr` → `schema`      | `event_name`             | Unpack the self-describing event, take the schema name (`page_view`). |
| `ue_pr` → `data`        | `payload` (JSON string)  | The body of the self-describing event, serialised as text.            |
| (the entire entry)      | a single item in `items` | Buffered client-side; flushed every 100 events or 5 s.                |

Self-describing events live under the `ue_pr` (unstructured event,
properties) field as a JSON string. Parse it once and split the
`schema` into a human-readable `event_name` so downstream queries can
filter without parsing JSON on every row.

## Schema

Create the destination collection once, before any inserts. The
adapter assumes a flat row shape so SQL queries stay first-class:

```sql
CREATE TABLE events (
  event_id          TEXT,
  collector_tstamp  INTEGER,
  event_name        TEXT,
  payload           TEXT
)
```

For high-volume ingestion, see the
[bulk insert performance notes](./api/http.md#bulk-insert-performance)
— the same fast path applies whether the producer is a tracker
adapter or any other client.

## The adapter (excerpt)

The first ten lines below are the shape the rest of the example fills
in. The full file is in
[`docs/examples/snowplow-adapter.mjs`](./examples/snowplow-adapter.mjs).

```js
#!/usr/bin/env node
// Snowplow tracker -> RedDB batch insert adapter.
// Translates a Snowplow `payload_data` array into rows for the
// `events` collection and POSTs them via /collections/events/bulk/rows
// (the batch insert endpoint). Referenced from
// docs/migrating-from-snowplow.md.

const REDDB_URL = process.env.REDDB_URL ?? 'http://127.0.0.1:5000';
const FLUSH_EVERY = Number(process.env.FLUSH_EVERY ?? 100);
const FLUSH_INTERVAL_MS = Number(process.env.FLUSH_INTERVAL_MS ?? 5_000);
```

Run it standalone (it ships a sample payload at the bottom of the
file):

```bash
node docs/examples/snowplow-adapter.mjs
```

## Idempotency

Every batch is POSTed with an `Idempotency-Key` header derived from
the sorted `event_id`s of the batch. The header is the wire-level
contract for "if you've seen this exact set of events before, don't
write them twice." On top of that, because each row carries the
tracker-issued `event_id` as its primary key, a re-sent batch is
deterministically deduplicated even if the header is dropped by an
intermediate proxy — `event_id` collisions are rejected by the
collection's uniqueness constraint when one is declared.

## What this guide does **not** cover

- **Schema validation against Iglu.** RedDB stores the self-describing
  body as opaque text. If you need Iglu schema enforcement, run it
  inside the adapter before calling `track()`.
- **Enrichment.** Geo-IP, UA parsing, currency conversion etc. live in
  the adapter or downstream of RedDB — out of scope here.
- **Bi-directional sync with an existing Snowplow data lake.** Treat
  RedDB as a new destination; back-fill historical data separately
  with the [data migration](./migrations/data-migrations.md) tools.
