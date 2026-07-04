# ADR 0067 — Document DML surface: inline JSON only, marker-as-assertion, clean break

**Status:** Accepted
**Date:** 2026-07-03
**Related:** [ADR 0066](0066-reserved-envelope-fields-user-pays.md) (reserved envelope fields), issue [#1668](https://github.com/reddb-io/reddb/issues/1668) (the walkthrough friction that triggered the review), glossary terms in `.red/context/data-model.md` (*JSON literal*, *`JSON_PARSE`*, *Array literal typing*, *Model marker*, *Identifier/data case rule*)

## Context

A design review of the Document model (2026-07-03, prompted by #1668)
found the DML surface accreted rather than designed:

- The documented body form was a quoted string interpreted as JSON by
  position (`VALUES ('{"a":1}')`), while a strictly better inline form
  (`VALUES ({"a":1})`) already existed in the lexer/parser, had
  conformance coverage, and was emitted by the Rust client — but was
  taught nowhere and not emitted by the JS/Python/PHP drivers.
- `INSERT INTO c DOCUMENT (body) VALUES (…)` carried a mandatory
  ceremonial column list whose only remaining functions were the magic
  `body` name and legacy `_ttl` metadata columns superseded by
  `WITH TTL`.
- The parser accepted things the runtime rejected (arbitrary column
  names in the document column list; dotted `SET a.b.c` paths), so
  users hit failures at runtime that belong at parse/analysis time.
- Model markers were inconsistent (`INSERT … DOCUMENT` singular
  mandatory; `UPDATE … DOCUMENTS` plural optional, defaulting to
  `ROWS`) and redundant: the catalog already knows every existing
  collection's model and enforces it on write.
- A bare all-numeric `[…]` literal committed to `Value::Vector` (f32)
  at parse time, silently corrupting large integers destined for JSON
  positions.
- Body-key case handling was accidentally asymmetric (case-insensitive
  column match on insert, exact match on read).

## Decision

1. **Inline strict-JSON literals are the only way to write JSON values
   in RQL.** `INSERT INTO events DOCUMENT VALUES ({"level": "info"})`.
   The quoted-string coercion (`VALUES ('{…}')`) is **rejected with a
   didactic error** naming the two sanctioned forms. Relaxed object
   syntax (`{key: 'v'}`) stays reserved for the Cypher property bag.
2. **`JSON_PARSE(<expr>)`** is added as the explicit string→JSON
   conversion for runtime-string cases (stringly columns, computed
   expressions). It is a utility, never the taught happy path.
3. **No deprecation window.** Published drivers emit the old quoted
   form; they are updated to emit inline literals in the same release
   the server starts rejecting it. Stated maintainer policy: at this
   stage RedDB carries no legacy acceptance paths — coherence is worth
   one coordinated release.
4. **Model markers exist only where they disambiguate what the catalog
   cannot know.** In INSERT, `DOCUMENT` is an optional **model
   assertion**: creates the collection with that model if absent,
   validates if present — the same statement bootstraps and repeats
   (idempotent). On existing collections the model is inferred from
   the catalog: `INSERT INTO events VALUES ({…})` and
   `UPDATE events SET user.address.city = 'SP'` need no marker.
   `UPDATE`'s `DOCUMENTS`/`ROWS`/`KV` markers are **removed**;
   `NODES`/`EDGES` **stay** (a graph collection holds both record
   kinds — only the user can say which).
5. **The document INSERT column list dies.** `(body)` is rejected with
   a didactic error; legacy `_ttl` metadata columns are rejected
   pointing at `WITH TTL`.
6. **Nested SQL SET is implemented, not excused.** Dotted paths deep-
   merge into the body; the HTTP JSON-patch endpoint remains for
   programmatic ops (unset, dry-run). RedDB's SQL must not do less
   than RedDB's HTTP.
7. **Array literals parse lossless.** Vector-vs-JSON is resolved by
   the analyzer/runtime from the target's type; the parse-time f32
   commitment is removed.
8. **Identifiers fold case; body keys never do.** JSON body keys are
   user data, matched exactly everywhere. The schemaless typo hazard
   (`userid` vs `UserId` → empty result) is a documentation concern,
   not a semantics lever.

## Consequences

- One coordinated release train: server grammar/runtime changes + all
  drivers (JS, js-client, Python, PHP — Rust already complies) + docs
  (quickstart, walkthrough, overview) + conformance fixtures move
  together.
- Docs teach exactly one form per operation. The quickstart/walkthrough
  bootstrap story unifies on the idempotent assertion form; explicit
  `CREATE DOCUMENT` remains for declarative style.
- Parse/analysis-time validation replaces runtime surprises: unknown
  document INSERT columns, wrong-model statements, and Postgres-isms
  (`CREATE TABLE … DOCUMENT (…)`, `->`/`->>`) get teaching errors that
  name the RedDB-native form.
- Existing scripts and stored statements using the removed forms break
  loudly at parse time with a message that shows the rewrite. This is
  accepted per the no-legacy policy (point 3).
- ADR 0066's error-quality budget extends to this surface: every
  removal lands with a didactic error, not a bare syntax error.

## Alternatives considered

1. **Deprecation window for the quoted form** (accept + warn for N
   releases) — rejected by maintainer policy: no legacy paths; the
   ecosystem emitters are all first-party, so a clean coordinated
   break is cheaper than carrying the coercion.
2. **Relaxed object literals** (`{kind: 'signup'}`) — rejected: the
   copy-paste use case is strict JSON by definition, and the syntax
   collides with the Cypher property bag.
3. **`JSON_PARSE` as the canonical form** (Terraform-style) — rejected:
   its argument is a quoted string, reintroducing the quotes in the
   happy path.
4. **Keeping `UPDATE … DOCUMENTS` as the dotted-path gate** — rejected:
   it is a parser shortcut, not semantics; the catalog knows the model.
