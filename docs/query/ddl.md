# DDL

RedDB DDL is collection-first. A collection is the named container, and its model
(`table`, `document`, `graph`, `vector`, `time_series`, `kv`, or `queue`) decides
which typed DDL forms are valid.

Use typed DDL when the caller knows the model and wants mismatch protection. Use
polymorphic DDL when an administrative workflow already resolved the target as a
collection and should dispatch through the catalog.

## Quickstart

```sql
CREATE TABLE users (id INT, email TEXT)
CREATE QUEUE user_jobs WORK
CREATE TIMESERIES cpu_metrics RETENTION 7 d

TRUNCATE TABLE users
TRUNCATE QUEUE user_jobs
TRUNCATE COLLECTION cpu_metrics

DROP TABLE IF EXISTS users
DROP COLLECTION IF EXISTS cpu_metrics
```

`TRUNCATE` removes all entities while preserving the collection contract and
indexes. `DROP` removes the collection, its contract, and collection-scoped
indexes. `IF EXISTS` makes missing targets a no-op success for both `DROP` and
`TRUNCATE`.

## Coverage

| Model | DROP | TRUNCATE | CREATE | ALTER |
|---|---|---|---|---|
| Collection (polymorphic) | `DROP COLLECTION [IF EXISTS] name` | `TRUNCATE COLLECTION [IF EXISTS] name` | Use a typed `CREATE` form | Use typed `ALTER` forms |
| Table | `DROP TABLE [IF EXISTS] name` | `TRUNCATE TABLE [IF EXISTS] name` | `CREATE TABLE [IF NOT EXISTS] name (...)` | `ALTER TABLE name ...` |
| Document | `DROP DOCUMENT [IF EXISTS] name` | `TRUNCATE DOCUMENT [IF EXISTS] name` | Implicit on document insert; table DDL can declare shared schema | Not model-specific yet |
| Graph | `DROP GRAPH [IF EXISTS] name` | `TRUNCATE GRAPH [IF EXISTS] name` | Implicit on graph writes | Not model-specific yet |
| Vector | `DROP VECTOR [IF EXISTS] name` | `TRUNCATE VECTOR [IF EXISTS] name` | Implicit on vector insert | Not model-specific yet |
| Time-series | `DROP TIMESERIES [IF EXISTS] name` | `TRUNCATE TIMESERIES [IF EXISTS] name` | `CREATE TIMESERIES [IF NOT EXISTS] name ...` | Retention policy commands |
| Key-value | `DROP KV [IF EXISTS] name` | `TRUNCATE KV [IF EXISTS] name` | Implicit on KV writes | Not model-specific yet |
| Queue | `DROP QUEUE [IF EXISTS] name` | `TRUNCATE QUEUE [IF EXISTS] name` | `CREATE QUEUE [IF NOT EXISTS] name [WORK|FANOUT] ...` | `ALTER QUEUE name SET MODE WORK|FANOUT` |
| Index | `DROP INDEX [IF EXISTS] name ON collection` | Not applicable | `CREATE INDEX [IF NOT EXISTS] name ON collection (...)` | Rebuild/enable operations are artifact management |
| View | `DROP VIEW [IF EXISTS] name` | Not applicable | `CREATE VIEW name AS ...` | `CREATE OR REPLACE VIEW name AS ...` |
| Schema | `DROP SCHEMA [IF EXISTS] name [CASCADE]` | Not applicable | `CREATE SCHEMA [IF NOT EXISTS] name` | Not model-specific |
| Sequence | `DROP SEQUENCE [IF EXISTS] name` | Not applicable | `CREATE SEQUENCE [IF NOT EXISTS] name ...` | Not model-specific |
| Probabilistic structures | `DROP HLL|SKETCH|FILTER [IF EXISTS] name` | Not applicable | `CREATE HLL|SKETCH|FILTER ...` | Command-specific |

The collection-model rows are the canonical DDL surface for persisted user data.
The remaining rows are supporting database objects that participate in the same
parser and authorization surface but are not collection entities.

## Typed vs Polymorphic

Typed DDL validates the catalog model before mutation:

```sql
DROP TABLE users
TRUNCATE VECTOR embeddings
DROP QUEUE IF EXISTS user_jobs
```

If `user_jobs` is a queue, `DROP TABLE user_jobs` fails with a model mismatch
instead of deleting the queue.

Polymorphic DDL resolves the target through `red.collections` and dispatches to
the correct typed implementation:

```sql
DROP COLLECTION users
TRUNCATE COLLECTION IF EXISTS embeddings
```

Use polymorphic DDL for admin tools, migrations, and cleanup jobs that operate
over catalog rows rather than hard-coded models.

## AI policy options

`CREATE TABLE ... WITH (...)` accepts per-collection AI policy clauses next to
`tenant_by` and `append_only`:

```sql
CREATE TABLE articles (id INT, title TEXT, body TEXT)
WITH (
  EMBED (fields = ('title', 'body'), provider = 'openai', model = 'text-embedding-3-small')
)
```

| Clause | Purpose | Status |
|---|---|---|
| `EMBED (...)` | Auto-embed declared fields over CDC | Available |
| `MODERATE (...)` | Content-moderation gate | Parses + persists; enforcement planned |
| `VISION (...)` | Image understanding from a reference field | Parses + persists; enforcement planned |

Each clause is validated against the provider modality matrix at `CREATE TABLE`
time, so a provider that cannot serve the requested modality is rejected up
front. See [Per-collection AI policy](ai-policy.md) for full grammar and
semantics.

## DROP Forms

```sql
DROP TABLE users
DROP GRAPH identity
DROP VECTOR notes
DROP DOCUMENT logs
DROP TIMESERIES metrics
DROP KV settings
DROP QUEUE tasks
DROP COLLECTION users

DROP TABLE IF EXISTS users
DROP COLLECTION IF EXISTS users
```

`DROP COLLECTION` is polymorphic. It deletes whichever model the catalog records
for the collection. Typed `DROP` only deletes when the model matches.

On an event-enabled collection, `DROP` emits one `collection_dropped` event with
`final_entities_count` before removing the source collection. Event queues are
preserved so consumers can drain pending messages.

## TRUNCATE Forms

```sql
TRUNCATE TABLE users
TRUNCATE GRAPH identity
TRUNCATE VECTOR notes
TRUNCATE DOCUMENT logs
TRUNCATE TIMESERIES metrics
TRUNCATE KV settings
TRUNCATE QUEUE tasks
TRUNCATE COLLECTION users

TRUNCATE TABLE IF EXISTS users
TRUNCATE COLLECTION IF EXISTS users
```

`TRUNCATE COLLECTION` is polymorphic. It preserves the collection contract,
declared columns, queue configuration, and collection-scoped indexes while
removing entities.

`TRUNCATE QUEUE tasks` is the canonical queue-emptying DDL. `QUEUE PURGE tasks`
remains a backward-compatible queue command alias and uses the same executor.

On an event-enabled collection, `TRUNCATE` emits one `truncate` event with
`entities_count`. It does not emit one delete event per entity.

## Authorization

In legacy role mode, `DROP` and `TRUNCATE` require a write-capable role. In IAM
mode, they also require explicit policy authorization on the resolved collection:

```sql
CREATE POLICY 'ddl-drop-users' AS '{"Statement":[{"Effect":"Allow","Action":["drop"],"Resource":["collection:users"]}]}'
CREATE POLICY 'ddl-truncate-deny' AS '{"Statement":[{"Effect":"Deny","Action":["truncate"],"Resource":["collection:users"]}]}'
SIMULATE alice ACTION drop ON 'collection:users'
```

For polymorphic `DROP COLLECTION` and `TRUNCATE COLLECTION`, missing-collection
resolution happens before the privilege decision; an absent collection reports a
not-found or `IF EXISTS` no-op rather than a policy denial.

## External Comparisons

| System | Drop | Empty while keeping definition | Polymorphic model dispatch |
|---|---|---|---|
| PostgreSQL | `DROP TABLE users` removes a table definition and data. | `TRUNCATE TABLE users` removes rows and keeps schema/index definitions. | No generic collection model; object type is part of the command. |
| MySQL | `DROP TABLE users` removes table metadata and data. | `TRUNCATE TABLE users` recreates/empties the table-like object and resets storage-dependent counters. | No generic collection model. |
| MongoDB | `db.collection.drop()` removes one collection. | `deleteMany({})` removes documents while keeping the collection; there is no SQL-style `TRUNCATE COLLECTION`. | Collection names are document collections; typed graph/vector/queue dispatch is outside the core command. |
| RedDB | Typed `DROP` plus polymorphic `DROP COLLECTION`. | Typed `TRUNCATE` plus polymorphic `TRUNCATE COLLECTION`; queues use `TRUNCATE QUEUE`. | Yes. The catalog model in `red.collections` selects the implementation. |

## Conformance Cases

The parser conformance corpus pins the DDL forms documented here:

```sql
DROP TABLE users
DROP GRAPH identity
DROP VECTOR notes
DROP DOCUMENT logs
DROP TIMESERIES metrics
DROP KV settings
DROP QUEUE tasks
DROP COLLECTION users
DROP COLLECTION IF EXISTS missing

TRUNCATE TABLE users
TRUNCATE GRAPH identity
TRUNCATE VECTOR notes
TRUNCATE DOCUMENT logs
TRUNCATE DOCUMENT IF EXISTS logs
TRUNCATE TIMESERIES metrics
TRUNCATE KV settings
TRUNCATE QUEUE tasks
TRUNCATE COLLECTION IF EXISTS users

QUEUE PURGE tasks

CREATE POLICY 'ddl-drop-users' AS '{"Statement":[{"Effect":"Allow","Action":["drop"],"Resource":["collection:users"]}]}'
CREATE POLICY 'ddl-truncate-deny' AS '{"Statement":[{"Effect":"Deny","Action":["truncate"],"Resource":["collection:users"]}]}'
SIMULATE alice ACTION drop ON 'collection:users'
```

## See Also

- [Data model overview](../data-models/overview.md)
- [`red.collections` schema reference](../reference/red-schema.md#redcollections)
- [Policies](../security/policies.md)
- [Event subscriptions](events.md)
- [Queues](../data-models/queues.md)
- [CREATE TABLE](create-table.md)
- [CREATE INDEX](create-index.md)
- [Maintenance and DDL extras](maintenance.md)
