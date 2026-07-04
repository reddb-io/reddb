# ADR 0066 — Reserved envelope field names are enforced against user data

**Status:** Accepted
**Date:** 2026-07-03
**Related:** issue [#1668](https://github.com/reddb-io/reddb/issues/1668) (Document walkthrough friction that surfaced the collision), `crates/reddb-server/src/reserved_fields.rs`

## Context

Every record RedDB returns — regardless of model — carries the system
envelope: `rid`, `collection`, `kind`, `tenant`, `created_at`,
`updated_at` (plus the internal moderation marker). These are also the
`FROM ANY` projection columns and appear unprefixed throughout the SQL
surface, docs, and drivers.

User data can collide with these names. For declared tables the
collision is cheap: it fails at `CREATE TABLE` time and the user picks
another column name. For documents it is expensive: the model is
schemaless ("bring your JSON as it is"), and real-world payloads
routinely carry top-level `kind`, `created_at`, `updated_at` (webhooks,
analytics events, data migrated from other document stores). The
rejection fires at write time, per insert.

The established document stores all resolved this the other way: the
**system** prefixes its fields (`_id`/`_rev` in CouchDB, `_id` in
MongoDB, `_source`/`_index` in Elasticsearch) and the user keeps the
natural namespace. Adopting that here would mean renaming the envelope
to `_rid`, `_kind`, `_created_at`, … — a breaking change to every
existing query, driver, doc, and the `FROM ANY` column set.

A third option — shadowing, where a colliding user field wins inside
its collection scope and the envelope stays reachable only via
`FROM ANY` or qualified names — avoids the breaking change but makes
`SELECT created_at` mean different things depending on the document.

## Decision

**The user pays for the collision.** Envelope field names stay
unprefixed and reserved at the top level of user data in every model;
a colliding user field is rejected at write time
(`reserved_fields.rs::ensure_no_reserved_public_item_fields`). Users
rename their data. RedDB deliberately diverges from the
underscore-prefix convention of MongoDB/CouchDB/Elasticsearch.

Rationale: RedDB is multi-model with one unified query surface. The
envelope vocabulary (`rid`, `kind`, `created_at`, …) is base RedDB
language used identically across tables, documents, graphs, queues and
`FROM ANY`; prefixing it for documents alone would fracture that
vocabulary, and prefixing it everywhere would break every existing
consumer for the benefit of one model.

The standing exception remains: declared tables with timestamps enabled
treat `created_at`/`updated_at` as the system-maintained columns rather
than rejecting them.

## Consequences

- Write-time rejection is a documented, permanent part of the document
  model's contract. The mitigation budget goes to **error quality and
  docs**, not to grammar or storage changes: the rejection message must
  name the offending field, state the reserved set, and point at the
  rename recourse.
- Docs and examples must never use reserved names as user body fields.
  (The original #1668 reply and walkthrough example used
  `{"kind": "signup", …}`, which this rule rejects — they need
  correcting, which is what surfaced this ADR.)
- Ingest pipelines from other stores need a field-rename step for
  colliding keys; this is accepted friction.
- Revisiting this later gets more expensive with every production
  insert made under the rule; treat the decision as settled.

## Alternatives considered

1. **Underscore-prefix the envelope** (`_rid`, `_kind`, …), industry
   convention — rejected: breaks every existing query/driver/doc and
   fractures the unprefixed cross-model vocabulary.
2. **Shadowing** (user field wins in collection scope) — rejected:
   the same column name silently changes meaning between collections
   and between scoped/`FROM ANY` reads.
